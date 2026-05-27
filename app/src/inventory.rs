//! 库存管理模块 — 物料 CRUD + 自动编码 + 入库/出库
//!
//! 编码规则（按分类前缀 + 年月 + 序号）：
//!   原材料 RM-2026-00001  成品 FG-2026-00001
//!   半成品 SF-2026-00001  包材 PK-2026-00001
//!   服务 SV-2026-00001    电子 EL-2026-00001

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use serde_json::{json, Value};
use crate::AppState;
use frappe_core::storage::{DocPagination, DocOrder};

/// 分类 → 编码前缀 映射
const CATEGORY_PREFIXES: &[(&str, &str)] = &[
    ("原材料", "RM"), ("成品", "FG"), ("半成品", "SF"),
    ("包材", "PK"), ("服务", "SV"), ("电子元器件", "EL"),
    ("电子设备", "EL"), ("机械设备", "ME"), ("办公用品", "OF"),
];

pub fn category_prefix(cat: &str) -> &'static str {
    CATEGORY_PREFIXES.iter()
        .find(|(k, _)| cat.contains(k) || k.contains(cat))
        .map(|(_, v)| *v)
        .unwrap_or("IT")
}

pub fn routes() -> Router<AppState> {
    Router::new()
        // 物料 CRUD
        .route("/api/inventory/item", get(item_list).post(item_create))
        .route("/api/inventory/item/:code", get(item_detail).put(item_update))
        // 自动编码预览
        .route("/api/inventory/code-preview", get(code_preview))
        // 库存查询
        .route("/api/inventory/stock", get(stock_list))
        .route("/api/inventory/stock/:item_code", get(stock_detail))
        // 入库 / 出库
        .route("/api/inventory/receipt", post(stock_receipt))
        .route("/api/inventory/issue", post(stock_issue))
}

fn ts() -> String { chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string() }
fn year_prefix() -> String { chrono::Utc::now().format("-%Y-").to_string() }

// ═══ 自动编码生成 ═══

/// 根据分类生成下一个物料编码，格式: PREFIX-YYYY-NNNNN
async fn generate_item_code(db: &AppState, category: &str) -> Result<String, StatusCode> {
    let prefix = category_prefix(category);
    let yp = year_prefix();
    let pattern = format!("{}{}", prefix, yp);
    // 搜索已有编码，取最大序号 + 1
    let p = DocPagination { limit: 9999, offset: 0 };
    let all = db.get_raw_list("Item", &[], &[], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut max_seq: u64 = 0;
    let search = format!("{}-", prefix);
    for item in &all {
        if let Some(code) = item.get("item_code").or(item.get("name")).and_then(|v| v.as_str()) {
            if code.starts_with(&search) {
                if let Some(seq_str) = code.rsplit('-').next() {
                    if let Ok(seq) = seq_str.parse::<u64>() {
                        max_seq = max_seq.max(seq);
                    }
                }
            }
        }
    }
    Ok(format!("{}{}{:05}", prefix, yp, max_seq + 1))
}

#[derive(serde::Deserialize)]
struct PreviewQ { category: Option<String> }

async fn code_preview(State(db): State<AppState>, Query(q): Query<PreviewQ>) -> Result<Json<Value>, StatusCode> {
    let cat = q.category.as_deref().unwrap_or("原材料");
    let code = generate_item_code(&db, cat).await?;
    let prefix = category_prefix(cat);
    // 返回编码规则说明
    let rules: Vec<Value> = CATEGORY_PREFIXES.iter().map(|(c, p)| {
        json!({"category": c, "prefix": p, "example": format!("{}-2026-00001", p)})
    }).collect();
    Ok(Json(json!({
        "next_code": code,
        "prefix": prefix,
        "category": cat,
        "pattern": format!("{}-YYYY-NNNNN (年月-5位序号)", prefix),
        "all_rules": rules,
    })))
}

// ═══ 物料 CRUD ═══

#[derive(serde::Deserialize)]
struct ItemQ { limit: Option<usize>, offset: Option<usize> }

async fn item_list(State(db): State<AppState>, Query(q): Query<ItemQ>) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: q.limit.unwrap_or(500), offset: q.offset.unwrap_or(0) };
    let order = DocOrder { field: "modified".into(), descending: true };
    let docs = db.get_raw_list("Item", &[], &[order], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let total = db.get_raw_count("Item").await.unwrap_or(0);
    Ok(Json(json!({"data":docs,"total":total})))
}

async fn item_detail(State(db): State<AppState>, Path(code): Path<String>) -> Result<Json<Value>, StatusCode> {
    db.get_raw_doc("Item", &code).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|d| Json(json!({"data":d}))).ok_or(StatusCode::NOT_FOUND)
}

async fn item_create(_auth: crate::middleware::AuthUser, State(db): State<AppState>, Json(body): Json<Value>) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let mut data = body.get("data").cloned().unwrap_or(body);
    let category = data["category"].as_str().unwrap_or("原材料");
    // 自动生成编码（可手动覆盖）
    let code = data["item_code"].as_str()
        .or(data["name"].as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            // 同步生成 — 在 handler 中不能 await，先给个 fallback
            format!("{}{}{:05}", category_prefix(category), year_prefix(), 1u64)
        });
    let final_code = if data["item_code"].as_str().map_or(true, |s| s.is_empty()) {
        generate_item_code(&db, category).await?
    } else {
        code
    };
    data["item_code"] = json!(final_code);
    data["name"] = json!(final_code);
    data["doctype"] = json!("Item");
    if data.get("stock_qty").is_none() { data["stock_qty"] = json!(0); }
    if data.get("unit").is_none() { data["unit"] = json!("个"); }
    data["created"] = json!(ts());
    db.insert_raw(&final_code, "Item", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"item_code":final_code,"doctype":"Item"}}))))
}

async fn item_update(_auth: crate::middleware::AuthUser, State(db): State<AppState>, Path(code): Path<String>, Json(body): Json<Value>) -> Result<Json<Value>, StatusCode> {
    let data = body.get("data").cloned().unwrap_or(body);
    db.save_raw(&code, "Item", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":{"item_code":code}})))
}

// ═══ 库存查询 (保持兼容) ═══

async fn stock_list(State(db): State<AppState>) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 500, offset: 0 };
    let items = db.get_raw_list("Item", &[], &[], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut stock_items = Vec::new();
    for item in &items {
        let code = item.get("item_code").or(item.get("name")).and_then(|v| v.as_str()).unwrap_or("");
        if code.is_empty() { continue; }
        let qty = item.get("stock_qty").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let up = item.get("unit_price").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let ss = item.get("safety_stock").and_then(|v| v.as_f64()).unwrap_or(5.0);
        let rl = item.get("reorder_level").and_then(|v| v.as_f64()).unwrap_or(10.0);
        stock_items.push(json!({"item_code":code,"item_name":item.get("item_name").and_then(|v| v.as_str()).unwrap_or(""),"category":item.get("category").and_then(|v| v.as_str()).unwrap_or(""),"unit":item.get("unit").and_then(|v| v.as_str()).unwrap_or("个"),"qty":qty,"stock_qty":qty,"unit_price":up,"rate":up,"total_value":qty*up,"safety_stock":ss,"reorder_level":rl,"warehouse":item.get("warehouse").and_then(|v| v.as_str()).unwrap_or("")}));
    }
    let total_qty: f64 = stock_items.iter().map(|i| i["stock_qty"].as_f64().unwrap_or(0.0)).sum();
    let total_value: f64 = stock_items.iter().map(|i| i["total_value"].as_f64().unwrap_or(0.0)).sum();
    Ok(Json(json!({"data":stock_items,"items":stock_items,"summary":{"total_items":stock_items.len(),"total_qty":total_qty,"total_value":total_value}})))
}

async fn stock_detail(State(db): State<AppState>, Path(item_code): Path<String>) -> Result<Json<Value>, StatusCode> {
    let doc = db.get_raw_doc("Item", &item_code).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.ok_or(StatusCode::NOT_FOUND)?;
    let p = DocPagination { limit: 200, offset: 0 };
    let order = DocOrder { field: "posting_date".into(), descending: true };
    let movements = db.get_raw_list("Stock Movement", &[frappe_core::storage::DocFilter::eq("item_code", item_code.clone())], &[order], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"item":doc,"movements":movements})))
}

// ═══ 入库/出库 (保持兼容) ═══

async fn stock_receipt(
    _auth: crate::middleware::AuthUser,
    State(db): State<AppState>, Json(body): Json<Value>) -> Result<Json<Value>, StatusCode> {
    let item_code = body["item_code"].as_str().unwrap_or("").to_string();
    if item_code.is_empty() { return Err(StatusCode::BAD_REQUEST); }
    let qty = body["qty"].as_f64().unwrap_or(0.0);
    if qty <= 0.0 { return Err(StatusCode::BAD_REQUEST); }
    let unit_price = body["unit_price"].as_f64().unwrap_or(0.0);
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let existing = db.get_raw_doc("Item", &item_code).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let is_new = existing.is_none();
    let mut item = existing.unwrap_or(json!({}));
    if is_new {
        item["item_code"] = json!(item_code); item["name"] = json!(item_code);
        item["item_name"] = body.get("item_name").cloned().unwrap_or(json!(""));
        item["category"] = body.get("category").cloned().unwrap_or(json!("原材料"));
        item["unit"] = body.get("unit").cloned().unwrap_or(json!("个"));
        item["warehouse"] = body.get("warehouse").cloned().unwrap_or(json!("主仓库"));
        item["doctype"] = json!("Item");
    }
    if unit_price > 0.0 { item["unit_price"] = json!(unit_price); }
    let old_qty = item.get("stock_qty").and_then(|v| v.as_f64()).unwrap_or(0.0);
    item["stock_qty"] = json!(old_qty + qty);
    if is_new { db.insert_raw(&item_code, "Item", &item).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?; }
    else { db.save_raw(&item_code, "Item", &item).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?; }
    let mv_name = format!("SM-RCPT-{}", chrono::Utc::now().format("%Y%m%d%H%M%S"));
    let mv = json!({"item_code":item_code,"item_name":body.get("item_name").cloned().unwrap_or(json!("")),"movement_type":"receipt","qty":qty,"unit_price":unit_price,"total_amount":qty*unit_price,"posting_date":&now[..10],"warehouse":body.get("warehouse").cloned().unwrap_or(json!("主仓库")),"remark":body.get("remark").cloned().unwrap_or(json!("")),"name":mv_name,"doctype":"Stock Movement"});
    db.insert_raw(&mv_name, "Stock Movement", &mv).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"message":format!("入库成功，+{} {}，当前库存 {}", qty, body.get("unit").and_then(|v| v.as_str()).unwrap_or("个"), old_qty+qty),"item_code":item_code,"qty":qty,"stock_after":old_qty+qty,"movement":mv_name})))
}

async fn stock_issue(
    _auth: crate::middleware::AuthUser,
    State(db): State<AppState>, Json(body): Json<Value>) -> Result<Json<Value>, StatusCode> {
    let item_code = body["item_code"].as_str().unwrap_or("").to_string();
    if item_code.is_empty() { return Err(StatusCode::BAD_REQUEST); }
    let qty = body["qty"].as_f64().unwrap_or(0.0);
    if qty <= 0.0 { return Err(StatusCode::BAD_REQUEST); }
    let mut item = db.get_raw_doc("Item", &item_code).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.ok_or(StatusCode::NOT_FOUND)?;
    let old_qty = item.get("stock_qty").and_then(|v| v.as_f64()).unwrap_or(0.0);
    if qty > old_qty {
        return Err(StatusCode::CONFLICT);
    }
    // 检查安全库存
    let safety = item.get("safety_stock").and_then(|v| v.as_f64()).unwrap_or(0.0);
    if old_qty - qty < safety {
        return Err(StatusCode::CONFLICT);
    }
    let unit_price = item.get("unit_price").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    item["stock_qty"] = json!(old_qty - qty);
    db.save_raw(&item_code, "Item", &item).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mv_name = format!("SM-ISSU-{}", chrono::Utc::now().format("%Y%m%d%H%M%S"));
    let mv = json!({"item_code":item_code,"item_name":item.get("item_name").cloned().unwrap_or(json!("")),"movement_type":"issue","qty":qty,"unit_price":unit_price,"total_amount":qty*unit_price,"posting_date":&now[..10],"warehouse":body.get("warehouse").cloned().unwrap_or(json!("主仓库")),"remark":body.get("remark").cloned().unwrap_or(json!("")),"name":mv_name,"doctype":"Stock Movement"});
    db.insert_raw(&mv_name, "Stock Movement", &mv).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"message":format!("出库成功，-{} {}，当前库存 {}", qty,item.get("unit").and_then(|v| v.as_str()).unwrap_or("个"),old_qty-qty),"item_code":item_code,"qty":qty,"stock_after":old_qty-qty,"movement":mv_name})))
}
