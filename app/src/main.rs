//! RustyERP v0.7.0 — REST API + Auth + MySQL + 审批工作流

mod auth;
mod api_auth;
mod middleware;
mod asset;
mod finance;
mod inventory;
mod production;
mod crm;
mod hr;
mod procurement;

mod export;

mod backup;
mod email;
mod tenant;

// 编译时生成的 doctype 元数据
include!(concat!(env!("OUT_DIR"), "/doctype_meta.rs"));

use crate::middleware::AuthUser;
use crate::auth::Permission;
use axum::{
    extract::{Path, Query, State},
    http::{header, Method, StatusCode},
    routing::{get, post, put},
    Json, Router,
};
use frappe_core::storage::{DocFilter, DocOrder, DocPagination};
use frappe_mysql::MysqlStorage;
use serde_json::{json, Value};
use tower_http::{cors::CorsLayer, services::ServeDir};

pub type AppState = MysqlStorage;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🦀 RustyERP v0.10.0 — API :8008 | 前端 :5184 | MySQL | 多级审批");

    let storage = MysqlStorage::new("mysql://rusty:Aa346402365@localhost/rusty_erp").await?;
    println!("🗄️ 数据库已连接 (MySQL 8.4)");

    let api = Router::new()
        .route("/health", get(health))
        .merge(api_auth::auth_routes())
        .route("/api/resource/:doctype", get(list).post(create))
        .route("/api/resource/:doctype/:name", get(get_one).put(update).delete(delete))
        .route("/api/resource/:doctype/:name/action", post(action))
        .route("/api/workflow/approve/:name", post(workflow_approve))
        .route("/api/workflow/reject/:name", post(workflow_reject))
        .route("/api/workflow/status/:name", get(workflow_status))
        .route("/api/doctype/:name/meta", get(doctype_meta))
        .route("/api/series/:pattern", get(next_series))
        .route("/api/link/:source_dt/:source_name/to/:target_dt", get(link_doc))
        // 固定资产
        .route("/api/asset/list", get(asset::asset_list))
        .route("/api/asset/create", post(asset::asset_create))
        .route("/api/asset/:name", put(asset::asset_update))
        .route("/api/asset/:name/depreciate", post(asset::asset_depreciate))
        .route("/api/asset/batch-depreciate", post(asset::asset_batch_depreciate))
        .route("/api/asset/:name/transfer", post(asset::asset_transfer))
        .route("/api/asset/:name/scrap", post(asset::asset_scrap))
        .route("/api/asset/:name/qrcode", get(asset::asset_qrcode))
        // 资产领用
        .route("/api/asset/requisition", get(asset::requisition_list).post(asset::requisition_create))
        .route("/api/asset/requisition/:name/approve", post(asset::requisition_approve))
        .route("/api/asset/requisition/:name/reject", post(asset::requisition_reject))
        .route("/api/asset/requisition/:name/check-out", post(asset::requisition_check_out))
        .route("/api/asset/requisition/:name/check-in", post(asset::requisition_check_in))
        // 扫码盘点
        .route("/api/asset/scan/:code", get(asset::scan_asset))
        .route("/api/asset/inventory-check", get(asset::inventory_check_list).post(asset::inventory_check_create))
        .route("/api/asset/inventory-check/:name/scan/:code", post(asset::inventory_check_scan))
        .route("/api/asset/inventory-check/:name/complete", post(asset::inventory_check_complete))
        .route("/api/asset/inventory-check/:name/report", get(asset::inventory_check_report))
        // 财务报表
        .merge(finance::routes())
        // 库存管理
        .merge(inventory::routes())
        // 生产管理
        .merge(production::routes())
        // CSV 导出
        .route("/api/export/:report_type", get(export::export_handler))
        // 审计日志
        .route("/api/audit", get(list_audit_log))
        // 数据备份
        .merge(backup::routes())
        // 邮件通知
        .merge(email::routes())
        // 多租户
        .merge(tenant::routes())
        .merge(crm::routes())
        // HR 人事
        .merge(hr::routes())
        // 采购供应链
        .merge(procurement::routes())
        .layer(
            CorsLayer::new()
                .allow_origin([
                    "http://localhost:5184".parse().unwrap(),
                    "http://127.0.0.1:5184".parse().unwrap(),
                    "http://192.168.201.230:5184".parse().unwrap(),
                ])
                .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]),
        )
        .with_state(storage.clone());

    // Clone for frontend too
    let _ = storage;

    let frontend = Router::new().nest_service("/", ServeDir::new(env!("CARGO_MANIFEST_DIR").to_string() + "/frontend"));

    println!("🚀 API: http://0.0.0.0:8008");
    let api_listener = tokio::net::TcpListener::bind("0.0.0.0:8008").await?;
    let api_handle = tokio::spawn(async {
        if let Err(e) = axum::serve(api_listener, api).await {
            eprintln!("❌ API 服务器致命错误: {e}");
        }
    });

    println!("🎨 前端: http://0.0.0.0:5184");
    let fe_listener = tokio::net::TcpListener::bind("0.0.0.0:5184").await?;
    axum::serve(fe_listener, frontend).await?;

    Ok(())
}

// ── Handlers ──

async fn health() -> Json<Value> {
    Json(json!({"name":"RustyERP","version":"0.10.0","status":"ok","backend":"MySQL","workflow":"multi-level"}))
}

#[derive(serde::Deserialize)]
struct Q {
    limit: Option<usize>,
    offset: Option<usize>,
    search: Option<String>,
    status: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct WfQ {
    doctype: String,
}

async fn list(
    _auth: AuthUser,
    State(db): State<AppState>,
    Path(doctype): Path<String>,
    Query(q): Query<Q>,
) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: q.limit.unwrap_or(100), offset: q.offset.unwrap_or(0) };
    let order = DocOrder { field: "modified".into(), descending: true };
    let mut filters: Vec<DocFilter> = Vec::new();
    if let Some(ref status) = q.status {
        let st: i32 = match status.as_str() { "draft"|"0" => 0, "submitted"|"1" => 1, "cancelled"|"2" => 2, _ => return Err(StatusCode::BAD_REQUEST) };
        filters.push(DocFilter::eq("docstatus", st.to_string()));
    }
    let (docs, total) = if let Some(ref search) = q.search {
        if search.is_empty() {
            (db.get_raw_list(&doctype, &filters, &[order.clone()], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
             db.get_raw_count(&doctype).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?)
        } else {
            (db.search_raw(&doctype, search, &[order.clone()], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
             db.get_raw_count_filtered(&doctype, search).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?)
        }
    } else {
        (db.get_raw_list(&doctype, &filters, &[order], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
         db.get_raw_count(&doctype).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?)
    };
    Ok(Json(json!({"data":docs,"total":total})))
}

async fn get_one(
    _auth: AuthUser, State(db): State<AppState>, Path((doctype, name)): Path<(String, String)>,
) -> Result<Json<Value>, StatusCode> {
    match db.get_raw_doc(&doctype, &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        Some(d) => Ok(Json(json!({"data":d}))),
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn create(
    _auth: AuthUser, State(db): State<AppState>,
    Path(doctype): Path<String>, Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let raw_name = body["name"].as_str().unwrap_or("").to_string();
    let name = if raw_name.starts_with("series:") {
        let pattern = &raw_name[7..];
        db.get_next_series(pattern).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        raw_name
    };
    let mut data = body.get("data").cloned().unwrap_or(body);
    data["name"] = json!(name);
    data["doctype"] = json!(doctype);
    db.insert_raw(&name, &doctype, &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"name":name,"doctype":doctype,"docstatus":0}}))))
}

async fn update(
    _auth: AuthUser, State(db): State<AppState>,
    Path((doctype, name)): Path<(String, String)>, Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    // 禁止编辑已提交/已取消的单据
    if let Some(doc) = db.get_raw_doc(&doctype, &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        let ds = doc.get("docstatus").and_then(|v| v.as_i64()).unwrap_or(0);
        if ds != 0 {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    let data = body.get("data").cloned().unwrap_or(body);
    db.save_raw(&name, &doctype, &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":{"name":name,"doctype":doctype}})))
}

async fn delete(
    auth: AuthUser, State(db): State<AppState>,
    Path((doctype, name)): Path<(String, String)>,
) -> Result<Json<Value>, StatusCode> {
    if !auth.can(Permission::Delete) { return Err(StatusCode::FORBIDDEN); }
    if let Some(doc) = db.get_raw_doc(&doctype, &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        let ds = doc.get("docstatus").and_then(|v| v.as_i64()).unwrap_or(0);
        if ds == 1 { return Err(StatusCode::FORBIDDEN); }
    }
    db.delete_raw(&doctype, &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"message":"已删除"})))
}

async fn action(
    auth: AuthUser, State(db): State<AppState>,
    Path((doctype, name)): Path<(String, String)>, Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let act = body["action"].as_str().unwrap_or("").to_lowercase();
    let st: i32 = match act.as_str() {
        "submit" => { if !auth.can(Permission::Submit) { return Err(StatusCode::FORBIDDEN); } 1 }
        "cancel" => { if !auth.can(Permission::Cancel) { return Err(StatusCode::FORBIDDEN); } 2 }
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    let mut doc = db.get_raw_doc(&doctype, &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    if st == 1 {
        // 提交：计算子表金额合计
        let mut grand_total = 0.0_f64;
        for key in &["items", "purchase_items"] {
            if let Some(items) = doc.get(key).and_then(|v| v.as_array()) {
                for item in items {
                    let qty = item.get("qty").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let rate = item.get("rate").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    grand_total += qty * rate;
                }
            }
        }
        if grand_total > 0.0 {
            doc["total"] = json!(grand_total);
            doc["grand_total"] = json!(grand_total);
        }
        if doc.get("posting_date").is_none() {
            doc["posting_date"] = json!(chrono::Utc::now().format("%Y-%m-%d").to_string());
        }
        doc["docstatus"] = json!(1);

        // 🆕 审批工作流：创建待审批记录
        let (levels, _approvers) = db.get_workflow_config(&doctype).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if levels > 0 {
            doc["approval_status"] = json!("pending");
            db.create_approvals(&doctype, &name, levels).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
    } else {
        doc["docstatus"] = json!(st);
    }

    db.save_raw(&name, &doctype, &doc).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":{"name":name,"doctype":doctype,"docstatus":st,"action":act}})))
}

// ── 审批工作流 API ──

/// 查看审批状态 (doctype from query param)
async fn workflow_status(
    _auth: AuthUser,
    Path(name): Path<String>,
    Query(q): Query<WfQ>,
    State(db): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    let doctype = q.doctype.clone();
    let doc = match db.get_raw_doc(&doctype, &name).await {
        Ok(Some(d)) => d,
        Ok(None) => return Err(StatusCode::NOT_FOUND),
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let astatus = doc.get("approval_status").and_then(|v| v.as_str()).unwrap_or("not_submitted");
    // 只测 get_approvals
    let (levels, approvers) = match db.get_workflow_config(&doctype).await {
        Ok(v) => v,
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let approvals = match db.get_approvals(&doctype, &name).await {
        Ok(v) => v,
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    Ok(Json(json!({
        "docname": name,
        "doctype": doctype,
        "approval_status": astatus,
        "total_levels": levels,
        "approvers": approvers,
        "approvals": approvals
    })))
}

/// 审批通过（自动找下一个 pending 级别，doctype from query param）
async fn workflow_approve(
    auth: AuthUser, State(db): State<AppState>,
    Path(name): Path<String>, Query(q): Query<WfQ>, Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let doctype = &q.doctype;
    let comment = body["comment"].as_str().unwrap_or("").to_string();

    // 找第一个 pending 级别
    let approvals = db.get_approvals(doctype, &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let next_level = approvals.iter()
        .find(|a| a.get("status").and_then(|s| s.as_str()) == Some("pending"))
        .and_then(|a| a.get("level").and_then(|l| l.as_i64()))
        .map(|l| l as i32);

    let level = next_level.ok_or(StatusCode::CONFLICT)?;

    let ok = db.approve_level(doctype, &name, level, &auth.username, &comment)
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !ok { return Err(StatusCode::CONFLICT); }

    // 检查是否全部通过
    let fully = db.is_fully_approved(doctype, &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut doc = db.get_raw_doc(doctype, &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    doc["approval_status"] = json!(if fully { "approved" } else { "pending" });
    db.save_raw(&name, doctype, &doc).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(json!({
        "message": format!("第 {} 级审批通过", level),
        "level": level,
        "fully_approved": fully
    })))
}

/// 驳回——退回草稿 (doctype from query param)
async fn workflow_reject(
    auth: AuthUser, State(db): State<AppState>,
    Path(name): Path<String>, Query(q): Query<WfQ>, Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let doctype = &q.doctype;
    let comment = body["comment"].as_str().unwrap_or("").to_string();
    let mut doc = db.get_raw_doc(doctype, &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    db.reject_all(doctype, &name, &auth.username, &comment)
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // 退回草稿
    doc["docstatus"] = json!(0);
    doc["approval_status"] = json!("rejected");
    db.save_raw(&name, doctype, &doc).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(json!({"message": "已驳回，退回草稿", "docname": name})))
}

async fn doctype_meta(Path(name): Path<String>) -> Result<Json<Value>, StatusCode> {
    match get_doctype_meta(&name) {
        Some(meta) => {
            let fields: Vec<Value> = meta.fields.iter().map(|f| json!({
                "fieldname": f.fieldname, "label": f.label, "fieldtype": f.fieldtype,
                "options": f.options, "required": f.required, "default": f.default_value,
            })).collect();
            Ok(Json(json!({"name":meta.name,"is_single":meta.is_single,"is_table":meta.is_table,"autoname":meta.autoname,"fields":fields})))
        }
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn next_series(
    Path(pattern): Path<String>,
    State(db): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    let name = db.get_next_series(&pattern).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"name": name, "pattern": pattern})))
}

async fn link_doc(
    Path((source_dt, source_name, target_dt)): Path<(String, String, String)>,
    State(db): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    let src = db.get_raw_doc(&source_dt, &source_name).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let mut data = serde_json::Map::new();
    for key in &["customer", "customer_name", "supplier", "supplier_name", "company", "currency"] {
        if let Some(v) = src.get(key) { data.insert(key.to_string(), v.clone()); }
    }
    for key in &["items", "purchase_items"] {
        if let Some(v) = src.get(key) { data.insert(key.to_string(), v.clone()); }
    }
    data.insert("from_doctype".into(), json!(source_dt));
    data.insert("from_name".into(), json!(source_name));

    Ok(Json(json!({
        "source": {"doctype": source_dt, "name": source_name},
        "target_doctype": target_dt,
        "data": data
    })))
}

/// 审计日志 — 列出所有 doctype 的最近操作
async fn list_audit_log(
    _auth: AuthUser,
    State(db): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    let doctypes = ["Sales Invoice","Purchase Invoice","Asset","Journal Entry","Item","BOM","Work Order","User"];
    let mut all_entries: Vec<Value> = Vec::new();
    for dt in &doctypes {
        let p = DocPagination { limit: 10, offset: 0 };
        if let Ok(docs) = db.get_raw_list(dt, &[], &[DocOrder{field:"modified".into(),descending:true}], &p).await {
            for d in docs {
                let status = match d.get("docstatus").and_then(|v| v.as_i64()).unwrap_or(0) {
                    0 => "草稿", 1 => "已提交", 2 => "已取消", _ => "草稿"
                };
                all_entries.push(json!({
                    "doctype": dt,
                    "name": d["name"],
                    "modified": d.get("modified").and_then(|v| v.as_str()).unwrap_or(""),
                    "docstatus": status,
                    "owner": d.get("owner").and_then(|v| v.as_str()).unwrap_or(""),
                }));
            }
        }
    }
    all_entries.sort_by(|a,b| {
        let am = a["modified"].as_str().unwrap_or("");
        let bm = b["modified"].as_str().unwrap_or("");
        bm.cmp(am) // newest first
    });
    all_entries.truncate(50); // top 50
    Ok(Json(json!({"data": all_entries})))
}
