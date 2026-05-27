//! 生产模块 — BOM管理 + 自动编码 + 工单
//!
//! BOM 编码规则: BOM-{产品编码}-V{版本号}
//!   例: BOM-FG-2026-00001-V1
//! 工单编码规则: WO-YYYY-NNNNN

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use serde_json::{json, Value};
use crate::AppState;
use crate::inventory;
use frappe_core::storage::{DocFilter, DocPagination, DocOrder};

pub fn routes() -> Router<AppState> {
    Router::new()
        // BOM 管理
        .route("/api/production/bom", get(bom_list).post(bom_create))
        .route("/api/production/bom/:name", get(bom_detail).put(bom_update))
        .route("/api/production/bom-code-preview", get(bom_code_preview))
        .route("/api/production/bom-check/:bom_name", get(check_bom))
        // 工单
        .route("/api/production/work-order", get(wo_list).post(wo_create))
        .route("/api/production/work-order/:name", get(wo_detail).put(wo_update))
        .route("/api/production/complete/:name", post(complete_work_order))
        .route("/api/production/cancel/:name", post(cancel_work_order))
}

fn ts() -> String { chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string() }
fn td() -> String { chrono::Utc::now().format("%Y-%m-%d").to_string() }

// ═══ BOM 编码 ═══

#[derive(serde::Deserialize)]
struct BomCodeQ { product_code: Option<String>, version: Option<u32> }

async fn bom_code_preview(State(db): State<AppState>, Query(q): Query<BomCodeQ>) -> Result<Json<Value>, StatusCode> {
    let product = q.product_code.as_deref().unwrap_or("FG-2026-00001");
    let ver = q.version.unwrap_or(1);
    let code = format!("BOM-{}-V{}", product, ver);
    // 检查是否已存在，自动递增版本
    let existing = db.get_raw_doc("BOM", &code).await.unwrap_or(None);
    let (final_code, final_ver) = if existing.is_some() {
        let v2 = ver + 1;
        (format!("BOM-{}-V{}", product, v2), v2)
    } else {
        (code, ver)
    };
    Ok(Json(json!({
        "next_code": final_code,
        "version": final_ver,
        "product_code": product,
        "pattern": "BOM-{产品编码}-V{版本号}",
        "tip": "版本号自动递增，也可手动指定",
    })))
}

// ═══ BOM CRUD ═══

async fn bom_list(State(db): State<AppState>) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 500, offset: 0 };
    let order = DocOrder { field: "modified".into(), descending: true };
    let docs = db.get_raw_list("BOM", &[], &[order], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let total = db.get_raw_count("BOM").await.unwrap_or(0);
    Ok(Json(json!({"data":docs,"total":total})))
}

async fn bom_detail(State(db): State<AppState>, Path(name): Path<String>) -> Result<Json<Value>, StatusCode> {
    db.get_raw_doc("BOM", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|d| Json(json!({"data":d}))).ok_or(StatusCode::NOT_FOUND)
}

async fn bom_create(_auth: crate::middleware::AuthUser, State(db): State<AppState>, Json(body): Json<Value>) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let mut data = body.get("data").cloned().unwrap_or(body);
    let product_code = data["product_code"].as_str().unwrap_or("");
    let version = data["version"].as_u64().unwrap_or(1) as u32;

    // 自动生成 BOM 编码
    let bom_code = data["name"].as_str()
        .or(data["bom_code"].as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            if product_code.is_empty() { "BOM-UNKNOWN-V1".into() }
            else { format!("BOM-{}-V{}", product_code, version) }
        });

    // 检查重复，自动递增版本
    let final_code = match db.get_raw_doc("BOM", &bom_code).await {
        Ok(Some(_)) => {
            let v = version + 1;
            if product_code.is_empty() { format!("BOM-V{}", v) }
            else { format!("BOM-{}-V{}", product_code, v) }
        }
        _ => bom_code,
    };

    data["name"] = json!(final_code);
    data["bom_code"] = json!(final_code);
    data["doctype"] = json!("BOM");
    if data.get("status").is_none() { data["status"] = json!("active"); }
    if data.get("items").is_none() { data["items"] = json!([]); }
    data["created"] = json!(ts());
    // 计算 BOM 成本
    let items_data = data.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let total_cost: f64 = items_data.iter().map(|i| {
        let qty = i.get("quantity").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let rate = i.get("rate").or(i.get("unit_price")).and_then(|v| v.as_f64()).unwrap_or(0.0);
        qty * rate
    }).sum();
    data["total_cost"] = json!(total_cost);

    db.insert_raw(&final_code, "BOM", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"name":final_code,"total_cost":total_cost}}))))
}

async fn bom_update(_auth: crate::middleware::AuthUser, State(db): State<AppState>, Path(name): Path<String>, Json(body): Json<Value>) -> Result<Json<Value>, StatusCode> {
    let mut data = body.get("data").cloned().unwrap_or(body);
    // 重算成本
    let items_data = data.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let total_cost: f64 = items_data.iter().map(|i| {
        let qty = i.get("quantity").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let rate = i.get("rate").or(i.get("unit_price")).and_then(|v| v.as_f64()).unwrap_or(0.0);
        qty * rate
    }).sum();
    data["total_cost"] = json!(total_cost);
    db.save_raw(&name, "BOM", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":{"name":name,"total_cost":total_cost}})))
}

// ═══ 工单 CRUD ═══

#[derive(serde::Deserialize)]
struct CheckQ { qty: Option<f64> }

async fn wo_list(State(db): State<AppState>) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 500, offset: 0 };
    let order = DocOrder { field: "modified".into(), descending: true };
    let docs = db.get_raw_list("WorkOrder", &[], &[order], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":docs,"total":docs.len()})))
}

async fn wo_detail(State(db): State<AppState>, Path(name): Path<String>) -> Result<Json<Value>, StatusCode> {
    db.get_raw_doc("WorkOrder", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|d| Json(json!({"data":d}))).ok_or(StatusCode::NOT_FOUND)
}

async fn wo_create(_auth: crate::middleware::AuthUser, State(db): State<AppState>, Json(body): Json<Value>) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let mut data = body.get("data").cloned().unwrap_or(body);
    let wo_id = db.get_next_series("WO-.YYYY.-").await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    data["name"] = json!(wo_id);
    data["doctype"] = json!("WorkOrder");
    data["status"] = json!("draft");
    data["created_date"] = json!(td());
    data["created"] = json!(ts());
    db.insert_raw(&wo_id, "WorkOrder", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"name":wo_id,"status":"draft"}}))))
}

async fn wo_update(_auth: crate::middleware::AuthUser, State(db): State<AppState>, Path(name): Path<String>, Json(body): Json<Value>) -> Result<Json<Value>, StatusCode> {
    let data = body.get("data").cloned().unwrap_or(body);
    db.save_raw(&name, "WorkOrder", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":{"name":name}})))
}

// ═══ BOM 检查 ═══

fn parse_bom_items(bom: &Value) -> Vec<BomItem> {
    bom.get("items").and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|i| {
            Some(BomItem {
                item_code: i.get("item_code").or(i.get("item_name")).and_then(|v| v.as_str()).unwrap_or("").to_string(),
                item_name: i.get("item_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                qty_per: i.get("quantity").and_then(|v| v.as_f64()).unwrap_or(0.0),
            })
        }).collect())
        .unwrap_or_default()
}

struct BomItem { item_code: String, item_name: String, qty_per: f64 }

async fn check_bom(State(db): State<AppState>, Path(bom_name): Path<String>, Query(q): Query<CheckQ>) -> Result<Json<Value>, StatusCode> {
    let bom = db.get_raw_doc("BOM", &bom_name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.ok_or(StatusCode::NOT_FOUND)?;
    let qty = q.qty.unwrap_or(1.0);
    let items = parse_bom_items(&bom);
    let mut results = Vec::new();
    let mut all_ok = true;
    for item in &items {
        let stock = db.get_raw_doc("Item", &item.item_code).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.map(|d| d.get("stock_qty").and_then(|v| v.as_f64()).unwrap_or(0.0)).unwrap_or(0.0);
        let needed = item.qty_per * qty;
        let ok = stock >= needed;
        if !ok { all_ok = false; }
        results.push(json!({"item_code":item.item_code,"item_name":item.item_name,"qty_needed":needed,"stock_qty":stock,"ok":ok,"shortage":if ok {0.0}else{needed-stock}}));
    }
    Ok(Json(json!({"bom":bom_name,"qty":qty,"all_ok":all_ok,"items":results})))
}

// ═══ 工单完成 — 自动扣料入库 ═══

async fn complete_work_order(
    _auth: crate::middleware::AuthUser,
    State(db): State<AppState>, Path(name): Path<String>) -> Result<Json<Value>, StatusCode> {
    let mut wo = db.get_raw_doc("WorkOrder", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.ok_or(StatusCode::NOT_FOUND)?;
    let bom_name = wo.get("bom_name").and_then(|v| v.as_str()).unwrap_or("");
    let wo_qty = wo.get("qty").and_then(|v| v.as_f64()).unwrap_or(1.0);

    // 获取 BOM 物料，扣减库存（检查材料是否充足）
    let mut deducted = Vec::new();
    if !bom_name.is_empty() {
        if let Ok(Some(bom)) = db.get_raw_doc("BOM", bom_name).await {
            // 先检查所有材料是否充足
            let mut shortages = Vec::new();
            for item in &parse_bom_items(&bom) {
                let needed = item.qty_per * wo_qty;
                if let Ok(Some(inv)) = db.get_raw_doc("Item", &item.item_code).await {
                    let stock = inv.get("stock_qty").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    if stock < needed {
                        shortages.push(format!("{}: 需要 {}, 库存 {}", item.item_code, needed, stock));
                    }
                }
            }
            if !shortages.is_empty() {
                return Err(StatusCode::CONFLICT);
            }
            // 扣减库存
            for item in &parse_bom_items(&bom) {
                let needed = item.qty_per * wo_qty;
                if let Ok(Some(mut inv)) = db.get_raw_doc("Item", &item.item_code).await {
                    let old = inv.get("stock_qty").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    inv["stock_qty"] = json!((old - needed).max(0.0));
                    db.save_raw(&item.item_code, "Item", &inv).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                    deducted.push(json!({"item_code":item.item_code,"qty_deducted":needed,"stock_after":(old-needed).max(0.0)}));
                }
            }
        }
    }

    // 增加成品库存
    let product_code = wo.get("product_code").and_then(|v| v.as_str()).unwrap_or("");
    if !product_code.is_empty() {
        if let Ok(Some(mut product)) = db.get_raw_doc("Item", product_code).await {
            let old = product.get("stock_qty").and_then(|v| v.as_f64()).unwrap_or(0.0);
            product["stock_qty"] = json!(old + wo_qty);
            db.save_raw(product_code, "Item", &product).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    }

    wo["status"] = json!("completed");
    wo["completed_date"] = json!(td());
    db.save_raw(&name, "WorkOrder", &wo).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(json!({"message":"工单已完成","data":{"name":name,"status":"completed","deducted":deducted}})))
}

async fn cancel_work_order(
    _auth: crate::middleware::AuthUser,
    State(db): State<AppState>, Path(name): Path<String>) -> Result<Json<Value>, StatusCode> {
    let mut wo = db.get_raw_doc("WorkOrder", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.ok_or(StatusCode::NOT_FOUND)?;
    wo["status"] = json!("cancelled");
    db.save_raw(&name, "WorkOrder", &wo).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"message":"工单已取消","data":{"name":name,"status":"cancelled"}})))
}
