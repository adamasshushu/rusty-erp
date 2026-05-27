//! 固定资产模块 — Depreciation Engine + QR + Lifecycle
//!
//! 资产状态：in_use → idle → maintenance → scrapped
//! 折旧方法：straight_line（直线法）/ double_declining（双倍余额递减）

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use crate::middleware::AuthUser;
use crate::AppState;
use frappe_core::storage::{DocOrder, DocPagination};
use serde_json::{json, Value};

/// 折旧计算：返回月折旧额
fn calc_monthly_depreciation(
    cost: f64,
    salvage: f64,
    useful_years: f64,
    accumulated: f64,
    method: &str,
) -> f64 {
    let months = useful_years * 12.0;
    if months <= 0.0 { return 0.0; }
    match method {
        "double_declining" => {
            let net = (cost - accumulated).max(0.0);
            if net <= salvage { return 0.0; }
            let rate = 2.0 / useful_years; // 年折旧率
            let monthly = (net * rate / 12.0).max(0.0);
            // 不低于直线法，不低于残值
            let sl = (cost - salvage) / months;
            monthly.max(sl).min(net - salvage)
        }
        _ => {
            // straight_line
            ((cost - salvage) / months).max(0.0)
        }
    }
}

/// 自动计算并更新资产折旧字段
fn update_depreciation_fields(doc: &mut Value) {
    let cost = doc.get("purchase_cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let salvage = doc.get("salvage_value").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let years = doc.get("useful_life_years").and_then(|v| v.as_f64()).unwrap_or(5.0);
    let accumulated = doc.get("accumulated_depreciation").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let method = doc.get("depreciation_method").and_then(|v| v.as_str()).unwrap_or("straight_line");

    let monthly = calc_monthly_depreciation(cost, salvage, years, accumulated, method);
    let net = (cost - accumulated).max(0.0);

    doc["monthly_depreciation"] = json!(monthly);
    doc["net_book_value"] = json!(net);
    if doc.get("depreciation_method").is_none() {
        doc["depreciation_method"] = json!("straight_line");
    }
    if doc.get("status").is_none() {
        doc["status"] = json!("in_use");
    }
}

// ── Handlers ──

/// 创建资产（重写：自动编号 + 折旧计算 + 生成标签数据）
pub async fn asset_create(
    auth: AuthUser, State(db): State<AppState>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let raw_name = body["name"].as_str().unwrap_or("").to_string();
    let name = if raw_name.is_empty() || raw_name.starts_with("series:") {
        let pattern = raw_name.strip_prefix("series:").unwrap_or("ASSET-.YYYY.-");
        db.get_next_series(pattern).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        raw_name
    };
    let mut data = body.get("data").cloned().unwrap_or(body);
    data["asset_code"] = json!(name);
    update_depreciation_fields(&mut data);
    // 生成 QR 标签数据
    data["qr_data"] = json!(serde_json::to_string(&json!({
        "type": "asset",
        "code": name,
        "name": data.get("asset_name").and_then(|v| v.as_str()).unwrap_or(""),
    })).unwrap_or_default());

    db.insert_raw(&name, "Asset", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"name":name,"doctype":"Asset","docstatus":0}}))))
}

/// 更新资产（重算折旧）
pub async fn asset_update(
    auth: AuthUser, State(db): State<AppState>,
    Path(name): Path<String>, Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    if let Some(doc) = db.get_raw_doc("Asset", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        let ds = doc.get("docstatus").and_then(|v| v.as_i64()).unwrap_or(0);
        if ds != 0 { return Err(StatusCode::FORBIDDEN); }
    }
    let mut data = body.get("data").cloned().unwrap_or(body);
    update_depreciation_fields(&mut data);
    db.save_raw(&name, "Asset", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":{"name":name,"doctype":"Asset"}})))
}

/// 获取资产列表（带折旧汇总）
pub async fn asset_list(
    State(db): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 500, offset: 0 };
    let order = DocOrder { field: "modified".into(), descending: true };
    let docs = db.get_raw_list("Asset", &[], &[order], &p)
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    // 汇总统计
    let mut total_cost = 0.0_f64;
    let mut total_net = 0.0_f64;
    let mut total_depreciation = 0.0_f64;
    let mut count_by_status = std::collections::HashMap::new();
    for d in &docs {
        total_cost += d.get("purchase_cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
        total_net += d.get("net_book_value").and_then(|v| v.as_f64()).unwrap_or(0.0);
        total_depreciation += d.get("accumulated_depreciation").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let st = d.get("status").and_then(|v| v.as_str()).unwrap_or("in_use");
        *count_by_status.entry(st.to_string()).or_insert(0) += 1;
    }
    Ok(Json(json!({
        "data": docs,
        "summary": {
            "total_count": docs.len(),
            "total_cost": total_cost,
            "total_net": total_net,
            "total_depreciation": total_depreciation,
            "by_status": count_by_status,
        }
    })))
}

/// 单资产折旧（手动触发一期折旧）
pub async fn asset_depreciate(
    auth: AuthUser, State(db): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let mut doc = db.get_raw_doc("Asset", &name).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let status = doc.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status == "scrapped" {
        return Err(StatusCode::BAD_REQUEST);
    }
    let cost = doc.get("purchase_cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let salvage = doc.get("salvage_value").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let years = doc.get("useful_life_years").and_then(|v| v.as_f64()).unwrap_or(5.0);
    let mut accumulated = doc.get("accumulated_depreciation").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let method = doc.get("depreciation_method").and_then(|v| v.as_str()).unwrap_or("straight_line");

    let monthly = calc_monthly_depreciation(cost, salvage, years, accumulated, method);
    if monthly <= 0.0 {
        return Ok(Json(json!({"message":"已提足折旧或无需计提","name":name,"monthly":0.0})));
    }
    accumulated += monthly;
    let net = (cost - accumulated).max(0.0);
    doc["accumulated_depreciation"] = json!(accumulated);
    doc["net_book_value"] = json!(net);
    doc["monthly_depreciation"] = json!(monthly);
    doc["last_depreciation_date"] = json!(chrono::Utc::now().format("%Y-%m-%d").to_string());

    db.save_raw(&name, "Asset", &doc).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({
        "message": format!("计提折旧 ¥{:.2}", monthly),
        "name": name,
        "monthly": monthly,
        "accumulated": accumulated,
        "net_book_value": net,
    })))
}

/// 批量月度折旧
pub async fn asset_batch_depreciate(
    auth: AuthUser, State(db): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 500, offset: 0 };
    let order = DocOrder { field: "modified".into(), descending: false };
    let docs = db.get_raw_list("Asset", &[], &[order], &p)
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut results = Vec::new();
    let mut total = 0.0_f64;
    for d in docs {
        let name = d.get("asset_code").or(d.get("name"))
            .and_then(|v| v.as_str()).unwrap_or("");
        if name.is_empty() { continue; }
        let status = d.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if status == "scrapped" { continue; }
        let cost = d.get("purchase_cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let salvage = d.get("salvage_value").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let years = d.get("useful_life_years").and_then(|v| v.as_f64()).unwrap_or(5.0);
        let accumulated = d.get("accumulated_depreciation").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let method = d.get("depreciation_method").and_then(|v| v.as_str()).unwrap_or("straight_line");
        let monthly = calc_monthly_depreciation(cost, salvage, years, accumulated, method);
        if monthly <= 0.0 { continue; }
        let new_acc = accumulated + monthly;
        let mut doc = d.clone();
        let idx = name.to_string();
        doc["accumulated_depreciation"] = json!(new_acc);
        doc["net_book_value"] = json!((cost - new_acc).max(0.0));
        doc["monthly_depreciation"] = json!(monthly);
        doc["last_depreciation_date"] = json!(chrono::Utc::now().format("%Y-%m-%d").to_string());
        db.save_raw(&name, "Asset", &doc).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        total += monthly;
        results.push(json!({"name": name, "monthly": monthly, "accumulated": new_acc}));
    }
    Ok(Json(json!({
        "message": format!("批量折旧完成，{} 项资产，合计 ¥{:.2}", results.len(), total),
        "count": results.len(),
        "total": total,
        "items": results,
    })))
}

/// 资产转移
pub async fn asset_transfer(
    auth: AuthUser, State(db): State<AppState>,
    Path(name): Path<String>, Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let mut doc = db.get_raw_doc("Asset", &name).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    if let Some(v) = body.get("department") { doc["department"] = v.clone(); }
    if let Some(v) = body.get("location") { doc["location"] = v.clone(); }
    if let Some(v) = body.get("custodian") { doc["custodian"] = v.clone(); }
    doc["last_transfer_date"] = json!(chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string());
    db.save_raw(&name, "Asset", &doc).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"message":"资产转移成功","name":name})))
}

/// 资产报废
pub async fn asset_scrap(
    auth: AuthUser, State(db): State<AppState>,
    Path(name): Path<String>, Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let mut doc = db.get_raw_doc("Asset", &name).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    doc["status"] = json!("scrapped");
    doc["scrap_date"] = json!(chrono::Utc::now().format("%Y-%m-%d").to_string());
    if let Some(v) = body.get("scrap_reason") { doc["scrap_reason"] = v.clone(); }
    db.save_raw(&name, "Asset", &doc).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"message":"资产已报废","name":name})))
}

/// 获取 QR 标签数据
pub async fn asset_qrcode(
    State(db): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let doc = db.get_raw_doc("Asset", &name).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(json!({
        "type": "asset",
        "code": name,
        "name": doc.get("asset_name").and_then(|v| v.as_str()).unwrap_or(""),
        "department": doc.get("department").and_then(|v| v.as_str()).unwrap_or(""),
        "location": doc.get("location").and_then(|v| v.as_str()).unwrap_or(""),
        "custodian": doc.get("custodian").and_then(|v| v.as_str()).unwrap_or(""),
        "status": doc.get("status").and_then(|v| v.as_str()).unwrap_or(""),
        "purchase_cost": doc.get("purchase_cost").and_then(|v| v.as_f64()).unwrap_or(0.0),
    })))
}

// ═══════════════════════════════════════════
//  资产领用 (Asset Requisition)
//  状态流转: pending → approved → checked_out → returned
// ═══════════════════════════════════════════

/// 领用单列表
pub async fn requisition_list(
    State(db): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 500, offset: 0 };
    let order = DocOrder { field: "modified".into(), descending: true };
    let docs = db.get_raw_list("AssetRequisition", &[], &[order], &p)
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data": docs, "total": docs.len()})))
}

/// 创建领用申请
pub async fn requisition_create(
    auth: AuthUser, State(db): State<AppState>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let mut data = body.get("data").cloned().unwrap_or(body);
    let name = data["name"].as_str()
        .or_else(|| data["asset_code"].as_str())
        .unwrap_or("").to_string();
    if name.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    // 自动生成领用编号
    let req_id = db.get_next_series("REQ-.YYYY.-")
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    data["name"] = json!(req_id);
    data["doctype"] = json!("AssetRequisition");
    data["status"] = json!("pending");
    data["asset_code"] = json!(name);
    data["requester"] = json!(auth.username);
    data["requested_date"] = json!(chrono::Utc::now().format("%Y-%m-%d").to_string());
    data["created"] = json!(chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string());
    db.insert_raw(&req_id, "AssetRequisition", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // 更新资产状态为申请中
    if let Some(mut asset) = db.get_raw_doc("Asset", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        asset["requisition_status"] = json!("pending");
        asset["current_requisition"] = json!(req_id);
        db.save_raw(&name, "Asset", &asset).await.ok();
    }

    Ok((StatusCode::CREATED, Json(json!({"data":{"name":req_id,"doctype":"AssetRequisition","status":"pending"}}))))
}

/// 审批领用
pub async fn requisition_approve(
    _auth: AuthUser, State(db): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let mut doc = db.get_raw_doc("AssetRequisition", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    doc["status"] = json!("approved");
    doc["approved_date"] = json!(chrono::Utc::now().format("%Y-%m-%d").to_string());
    db.save_raw(&name, "AssetRequisition", &doc).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // 更新资产状态
    if let Some(asset_code) = doc.get("asset_code").and_then(|v| v.as_str()) {
        if let Some(mut asset) = db.get_raw_doc("Asset", asset_code).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
            asset["requisition_status"] = json!("approved");
            db.save_raw(asset_code, "Asset", &asset).await.ok();
        }
    }

    Ok(Json(json!({"message":"已审批","data":{"name":name,"status":"approved"}})))
}

/// 驳回领用
pub async fn requisition_reject(
    _auth: AuthUser, State(db): State<AppState>,
    Path(name): Path<String>, Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let mut doc = db.get_raw_doc("AssetRequisition", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    doc["status"] = json!("rejected");
    doc["reject_reason"] = json!(body.get("reason").and_then(|v| v.as_str()).unwrap_or(""));
    db.save_raw(&name, "AssetRequisition", &doc).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // 释放资产
    if let Some(asset_code) = doc.get("asset_code").and_then(|v| v.as_str()) {
        if let Some(mut asset) = db.get_raw_doc("Asset", asset_code).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
            asset["requisition_status"] = json!("available");
            asset["current_requisition"] = json!("");
            db.save_raw(asset_code, "Asset", &asset).await.ok();
        }
    }

    Ok(Json(json!({"message":"已驳回","data":{"name":name,"status":"rejected"}})))
}

/// 确认出库（领用人领取资产）
pub async fn requisition_check_out(
    _auth: AuthUser, State(db): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let mut doc = db.get_raw_doc("AssetRequisition", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    doc["status"] = json!("checked_out");
    doc["checked_out_date"] = json!(chrono::Utc::now().format("%Y-%m-%d").to_string());
    db.save_raw(&name, "AssetRequisition", &doc).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // 更新资产状态为使用中
    if let Some(asset_code) = doc.get("asset_code").and_then(|v| v.as_str()) {
        if let Some(mut asset) = db.get_raw_doc("Asset", asset_code).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
            asset["status"] = json!("in_use");
            asset["requisition_status"] = json!("checked_out");
            asset["custodian"] = json!(doc.get("requester").and_then(|v| v.as_str()).unwrap_or(""));
            db.save_raw(asset_code, "Asset", &asset).await.ok();
        }
    }

    Ok(Json(json!({"message":"已出库","data":{"name":name,"status":"checked_out"}})))
}

/// 确认归还
pub async fn requisition_check_in(
    _auth: AuthUser, State(db): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let mut doc = db.get_raw_doc("AssetRequisition", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    doc["status"] = json!("returned");
    doc["returned_date"] = json!(chrono::Utc::now().format("%Y-%m-%d").to_string());
    db.save_raw(&name, "AssetRequisition", &doc).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // 恢复资产为可用
    if let Some(asset_code) = doc.get("asset_code").and_then(|v| v.as_str()) {
        if let Some(mut asset) = db.get_raw_doc("Asset", asset_code).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
            asset["status"] = json!("available");
            asset["requisition_status"] = json!("available");
            asset["current_requisition"] = json!("");
            db.save_raw(asset_code, "Asset", &asset).await.ok();
        }
    }

    Ok(Json(json!({"message":"已归还","data":{"name":name,"status":"returned"}})))
}

// ═══════════════════════════════════════════
//  扫码盘点 (QR Code Inventory Check)
// ═══════════════════════════════════════════

/// 扫码验证（通过 QR 码查找资产）
pub async fn scan_asset(
    State(db): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    // 通过资产编码查找
    let asset = db.get_raw_doc("Asset", &code).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .or_else(|| {
            // 如果精确匹配失败，尝试模糊搜索
            None
        });

    match asset {
        Some(doc) => {
            let status = doc.get("status").and_then(|v| v.as_str()).unwrap_or("unknown");
            let location = doc.get("location").and_then(|v| v.as_str()).unwrap_or("");
            Ok(Json(json!({
                "found": true,
                "code": code,
                "asset_name": doc.get("asset_name").and_then(|v| v.as_str()).unwrap_or(""),
                "category": doc.get("category").and_then(|v| v.as_str()).unwrap_or(""),
                "status": status,
                "location": location,
                "purchase_cost": doc.get("purchase_cost").and_then(|v| v.as_f64()).unwrap_or(0.0),
            })))
        }
        None => Ok(Json(json!({
            "found": false,
            "code": code,
            "message": "未找到该资产"
        }))),
    }
}

/// 盘点任务列表
pub async fn inventory_check_list(
    State(db): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 500, offset: 0 };
    let order = DocOrder { field: "modified".into(), descending: true };
    let docs = db.get_raw_list("InventoryCheck", &[], &[order], &p)
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data": docs, "total": docs.len()})))
}

/// 创建盘点任务
pub async fn inventory_check_create(
    auth: AuthUser, State(db): State<AppState>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let mut data = body.get("data").cloned().unwrap_or(body);
    let check_id = db.get_next_series("INV-.YYYY.-")
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    data["name"] = json!(check_id);
    data["doctype"] = json!("InventoryCheck");
    data["status"] = json!("in_progress");
    data["checker"] = json!(auth.username);
    data["start_date"] = json!(chrono::Utc::now().format("%Y-%m-%d").to_string());
    data["created"] = json!(chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string());
    if data.get("scanned_items").is_none() { data["scanned_items"] = json!([]); }
    if data.get("location").is_none() { data["location"] = json!("全部区域"); }
    db.insert_raw(&check_id, "InventoryCheck", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"name":check_id,"doctype":"InventoryCheck","status":"in_progress"}}))))
}

/// 盘点中扫码添加
pub async fn inventory_check_scan(
    _auth: AuthUser, State(db): State<AppState>,
    Path((check_id, code)): Path<(String, String)>,
) -> Result<Json<Value>, StatusCode> {
    let mut check = db.get_raw_doc("InventoryCheck", &check_id).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    // 扫码查找资产
    let asset = db.get_raw_doc("Asset", &code).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let scan_result = match &asset {
        Some(doc) => json!({
            "code": code,
            "name": doc.get("asset_name").and_then(|v| v.as_str()).unwrap_or(""),
            "location": doc.get("location").and_then(|v| v.as_str()).unwrap_or(""),
            "status": "found",
            "system_location": doc.get("location").and_then(|v| v.as_str()).unwrap_or(""),
            "match": true,
        }),
        None => json!({
            "code": code,
            "status": "extra",
            "match": false,
            "message": "系统无此资产记录",
        }),
    };

    // 添加到 scanned_items
    let mut items = check.get("scanned_items")
        .and_then(|v| v.as_array()).cloned().unwrap_or_default();
    items.push(scan_result.clone());
    check["scanned_items"] = json!(items);
    check["modified"] = json!(chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string());
    db.save_raw(&check_id, "InventoryCheck", &check).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(json!({"data": scan_result, "total_scanned": items.len()})))
}

/// 完成盘点，生成报告
pub async fn inventory_check_complete(
    _auth: AuthUser, State(db): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let mut check = db.get_raw_doc("InventoryCheck", &name).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let items = check.get("scanned_items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let found = items.iter().filter(|i| i.get("status").and_then(|v| v.as_str()) == Some("found")).count();
    let extra = items.iter().filter(|i| i.get("status").and_then(|v| v.as_str()) == Some("extra")).count();
    let missing = items.iter().filter(|i| i.get("status").and_then(|v| v.as_str()) == Some("missing")).count();

    check["status"] = json!("completed");
    check["end_date"] = json!(chrono::Utc::now().format("%Y-%m-%d").to_string());
    check["summary"] = json!({
        "total_scanned": items.len(),
        "found": found,
        "extra": extra,
        "missing": missing,
    });
    db.save_raw(&name, "InventoryCheck", &check).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(json!({
        "message": "盘点已完成",
        "data": {
            "name": name,
            "total_scanned": items.len(),
            "found": found,
            "extra": extra,
            "missing": missing,
        }
    })))
}

/// 获取盘点报告
pub async fn inventory_check_report(
    State(db): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let check = db.get_raw_doc("InventoryCheck", &name).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let items = check.get("scanned_items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let summary = check.get("summary").cloned().unwrap_or(json!({}));

    Ok(Json(json!({
        "name": name,
        "checker": check.get("checker").and_then(|v| v.as_str()).unwrap_or(""),
        "location": check.get("location").and_then(|v| v.as_str()).unwrap_or(""),
        "status": check.get("status").and_then(|v| v.as_str()).unwrap_or(""),
        "start_date": check.get("start_date").and_then(|v| v.as_str()).unwrap_or(""),
        "end_date": check.get("end_date").and_then(|v| v.as_str()).unwrap_or(""),
        "items": items,
        "summary": summary,
    })))
}
