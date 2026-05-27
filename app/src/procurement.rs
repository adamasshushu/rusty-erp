//! 采购供应链模块 — 供应商 / 采购申请 / 采购订单 / 收货入库
//!
//! 流程: PurchaseRequisition → PurchaseOrder → GoodsReceipt → 关联库存
//! 状态: pending → approved → ordered → received → invoiced

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use crate::middleware::AuthUser;
use crate::AppState;
use frappe_core::storage::{DocFilter, DocOrder, DocPagination};
use serde_json::{json, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        // 供应商
        .route("/api/procurement/supplier", get(supplier_list).post(supplier_create))
        .route("/api/procurement/supplier/:name", get(supplier_get).put(supplier_update))
        // 采购申请
        .route("/api/procurement/pr", get(pr_list).post(pr_create))
        .route("/api/procurement/pr/:name", get(pr_get).put(pr_update))
        .route("/api/procurement/pr/:name/approve", post(pr_approve))
        .route("/api/procurement/pr/:name/reject", post(pr_reject))
        // 采购订单
        .route("/api/procurement/po", get(po_list).post(po_create))
        .route("/api/procurement/po/:name", get(po_get).put(po_update))
        .route("/api/procurement/po/:name/order", post(po_order))
        .route("/api/procurement/po/:name/receive", post(po_receive))
        // 收货单
        .route("/api/procurement/receipt", get(receipt_list).post(receipt_create))
        .route("/api/procurement/receipt/:name", get(receipt_get).put(receipt_update))
        // 统计
        .route("/api/procurement/stats", get(procurement_stats))
}

fn ts() -> String { chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string() }
fn td() -> String { chrono::Utc::now().format("%Y-%m-%d").to_string() }

// ═══ 供应商 ═══

async fn supplier_list(State(db): State<AppState>) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 500, offset: 0 };
    let order = DocOrder { field: "modified".into(), descending: true };
    let docs = db.get_raw_list("Supplier", &[], &[order], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let total = db.get_raw_count("Supplier").await.unwrap_or(0);
    Ok(Json(json!({"data":docs,"total":total})))
}

async fn supplier_get(State(db): State<AppState>, Path(name): Path<String>) -> Result<Json<Value>, StatusCode> {
    db.get_raw_doc("Supplier", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|d| Json(json!({"data":d}))).ok_or(StatusCode::NOT_FOUND)
}

async fn supplier_create(_auth: AuthUser, State(db): State<AppState>, Json(body): Json<Value>) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let mut data = body.get("data").cloned().unwrap_or(body);
    let name = data["name"].as_str().or(data["supplier_name"].as_str()).unwrap_or("").to_string();
    if name.is_empty() { return Err(StatusCode::BAD_REQUEST); }
    data["name"] = json!(name);
    data["doctype"] = json!("Supplier");
    data["status"] = json!("active");
    data["created"] = json!(ts());
    db.insert_raw(&name, "Supplier", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"name":name}}))))
}

async fn supplier_update(_auth: AuthUser, State(db): State<AppState>, Path(name): Path<String>, Json(body): Json<Value>) -> Result<Json<Value>, StatusCode> {
    let data = body.get("data").cloned().unwrap_or(body);
    db.save_raw(&name, "Supplier", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":{"name":name}})))
}

// ═══ 采购申请 (Purchase Requisition) ═══

async fn pr_list(State(db): State<AppState>) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 500, offset: 0 };
    let order = DocOrder { field: "modified".into(), descending: true };
    let docs = db.get_raw_list("PurchaseRequisition", &[], &[order], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":docs,"total":docs.len()})))
}

async fn pr_get(State(db): State<AppState>, Path(name): Path<String>) -> Result<Json<Value>, StatusCode> {
    db.get_raw_doc("PurchaseRequisition", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|d| Json(json!({"data":d}))).ok_or(StatusCode::NOT_FOUND)
}

async fn pr_create(_auth: AuthUser, State(db): State<AppState>, Json(body): Json<Value>) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let mut data = body.get("data").cloned().unwrap_or(body);
    let pr_id = db.get_next_series("PR-.YYYY.-").await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    data["name"] = json!(pr_id);
    data["doctype"] = json!("PurchaseRequisition");
    data["status"] = json!("pending");
    data["requested_date"] = json!(td());
    data["created"] = json!(ts());
    if data.get("items").is_none() { data["items"] = json!([]); }
    db.insert_raw(&pr_id, "PurchaseRequisition", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"name":pr_id,"status":"pending"}}))))
}

async fn pr_update(_auth: AuthUser, State(db): State<AppState>, Path(name): Path<String>, Json(body): Json<Value>) -> Result<Json<Value>, StatusCode> {
    let data = body.get("data").cloned().unwrap_or(body);
    db.save_raw(&name, "PurchaseRequisition", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":{"name":name}})))
}

async fn pr_approve(_auth: AuthUser, State(db): State<AppState>, Path(name): Path<String>) -> Result<Json<Value>, StatusCode> {
    let mut doc = db.get_raw_doc("PurchaseRequisition", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.ok_or(StatusCode::NOT_FOUND)?;
    doc["status"] = json!("approved");
    doc["approved_date"] = json!(td());
    db.save_raw(&name, "PurchaseRequisition", &doc).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"message":"已审批","data":{"name":name,"status":"approved"}})))
}

async fn pr_reject(_auth: AuthUser, State(db): State<AppState>, Path(name): Path<String>) -> Result<Json<Value>, StatusCode> {
    let mut doc = db.get_raw_doc("PurchaseRequisition", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.ok_or(StatusCode::NOT_FOUND)?;
    doc["status"] = json!("rejected");
    db.save_raw(&name, "PurchaseRequisition", &doc).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"message":"已驳回","data":{"name":name,"status":"rejected"}})))
}

// ═══ 采购订单 (Purchase Order) ═══

async fn po_list(State(db): State<AppState>) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 500, offset: 0 };
    let order = DocOrder { field: "modified".into(), descending: true };
    let docs = db.get_raw_list("PurchaseOrder", &[], &[order], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":docs,"total":docs.len()})))
}

async fn po_get(State(db): State<AppState>, Path(name): Path<String>) -> Result<Json<Value>, StatusCode> {
    db.get_raw_doc("PurchaseOrder", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|d| Json(json!({"data":d}))).ok_or(StatusCode::NOT_FOUND)
}

async fn po_create(_auth: AuthUser, State(db): State<AppState>, Json(body): Json<Value>) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let mut data = body.get("data").cloned().unwrap_or(body);
    let po_id = db.get_next_series("PO-.YYYY.-").await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    data["name"] = json!(po_id);
    data["doctype"] = json!("PurchaseOrder");
    data["status"] = json!("draft");
    data["order_date"] = json!(td());
    data["created"] = json!(ts());
    if data.get("items").is_none() { data["items"] = json!([]); }
    db.insert_raw(&po_id, "PurchaseOrder", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"name":po_id,"status":"draft"}}))))
}

async fn po_update(_auth: AuthUser, State(db): State<AppState>, Path(name): Path<String>, Json(body): Json<Value>) -> Result<Json<Value>, StatusCode> {
    let data = body.get("data").cloned().unwrap_or(body);
    db.save_raw(&name, "PurchaseOrder", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":{"name":name}})))
}

/// 下单
async fn po_order(_auth: AuthUser, State(db): State<AppState>, Path(name): Path<String>) -> Result<Json<Value>, StatusCode> {
    let mut doc = db.get_raw_doc("PurchaseOrder", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.ok_or(StatusCode::NOT_FOUND)?;
    doc["status"] = json!("ordered");
    doc["ordered_date"] = json!(td());
    db.save_raw(&name, "PurchaseOrder", &doc).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"message":"已下单","data":{"name":name,"status":"ordered"}})))
}

/// 收货
async fn po_receive(_auth: AuthUser, State(db): State<AppState>, Path(name): Path<String>) -> Result<Json<Value>, StatusCode> {
    let mut doc = db.get_raw_doc("PurchaseOrder", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.ok_or(StatusCode::NOT_FOUND)?;
    doc["status"] = json!("received");
    doc["received_date"] = json!(td());
    db.save_raw(&name, "PurchaseOrder", &doc).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // 自动更新库存（如果有关联物料）
    if let Some(items) = doc.get("items").and_then(|v| v.as_array()) {
        for item in items {
            if let (Some(item_name), Some(qty)) = (item.get("item_name").and_then(|v| v.as_str()), item.get("quantity").and_then(|v| v.as_f64())) {
                // 更新库存
                if let Ok(Some(mut inventory)) = db.get_raw_doc("Item", item_name).await {
                    let current = inventory.get("stock_qty").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    inventory["stock_qty"] = json!(current + qty);
                    db.save_raw(item_name, "Item", &inventory).await.ok();
                }
            }
        }
    }

    Ok(Json(json!({"message":"已收货","data":{"name":name,"status":"received"}})))
}

// ═══ 收货单 (Goods Receipt) ═══

async fn receipt_list(State(db): State<AppState>) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 500, offset: 0 };
    let order = DocOrder { field: "modified".into(), descending: true };
    let docs = db.get_raw_list("GoodsReceipt", &[], &[order], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":docs,"total":docs.len()})))
}

async fn receipt_get(State(db): State<AppState>, Path(name): Path<String>) -> Result<Json<Value>, StatusCode> {
    db.get_raw_doc("GoodsReceipt", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|d| Json(json!({"data":d}))).ok_or(StatusCode::NOT_FOUND)
}

async fn receipt_create(_auth: AuthUser, State(db): State<AppState>, Json(body): Json<Value>) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let mut data = body.get("data").cloned().unwrap_or(body);
    let gr_id = db.get_next_series("GRN-.YYYY.-").await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    data["name"] = json!(gr_id);
    data["doctype"] = json!("GoodsReceipt");
    data["status"] = json!("draft");
    data["receipt_date"] = json!(td());
    data["created"] = json!(ts());
    if data.get("items").is_none() { data["items"] = json!([]); }
    db.insert_raw(&gr_id, "GoodsReceipt", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"name":gr_id,"status":"draft"}}))))
}

async fn receipt_update(_auth: AuthUser, State(db): State<AppState>, Path(name): Path<String>, Json(body): Json<Value>) -> Result<Json<Value>, StatusCode> {
    let data = body.get("data").cloned().unwrap_or(body);
    db.save_raw(&name, "GoodsReceipt", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":{"name":name}})))
}

// ═══ 采购统计 ═══

async fn procurement_stats(State(db): State<AppState>) -> Result<Json<Value>, StatusCode> {
    let total_suppliers = db.get_raw_count("Supplier").await.unwrap_or(0);
    let p = DocPagination { limit: 9999, offset: 0 };
    let pos = db.get_raw_list("PurchaseOrder", &[], &[], &p).await.unwrap_or_default();
    let prs = db.get_raw_list("PurchaseRequisition", &[], &[], &p).await.unwrap_or_default();
    let ordered = pos.iter().filter(|po| po.get("status").and_then(|v| v.as_str()) == Some("ordered")).count();
    let received = pos.iter().filter(|po| po.get("status").and_then(|v| v.as_str()) == Some("received")).count();
    let pending_prs = prs.iter().filter(|pr| pr.get("status").and_then(|v| v.as_str()) == Some("pending")).count();
    let mut total_po_value = 0.0_f64;
    for po in &pos {
        if let Some(items) = po.get("items").and_then(|v| v.as_array()) {
            for item in items {
                let qty = item.get("quantity").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let rate = item.get("rate").and_then(|v| v.as_f64()).unwrap_or(0.0);
                total_po_value += qty * rate;
            }
        }
    }
    Ok(Json(json!({
        "total_suppliers": total_suppliers,
        "total_pr": prs.len(),
        "pending_pr": pending_prs,
        "total_po": pos.len(),
        "ordered_po": ordered,
        "received_po": received,
        "total_po_value": format!("{:.2}", total_po_value),
    })))
}
