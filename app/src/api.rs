//! RustyERP HTTP REST API
//!
//! Frappe 风格 REST 端点：
//!   GET    /api/resource/{doctype}              → 列表
//!   GET    /api/resource/{doctype}/{name}        → 获取单个
//!   POST   /api/resource/{doctype}              → 创建
//!   PUT    /api/resource/{doctype}/{name}        → 更新
//!   DELETE /api/resource/{doctype}/{name}        → 删除
//!   POST   /api/resource/{doctype}/{name}/action → 提交/取消

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;

use frappe_core::storage::{DocOrder, DocPagination};
use frappe_sqlite::SqliteStorage;

/// 共享应用状态
pub struct AppState {
    pub db: Mutex<SqliteStorage>,
}

/// 构建路由
pub fn build_router(state: AppState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/resource/{doctype}", get(list_docs).post(create_doc))
        .route(
            "/api/resource/{doctype}/{name}",
            get(get_doc).put(update_doc).delete(delete_doc),
        )
        .route("/api/resource/{doctype}/{name}/action", post(do_action))
        .route("/health", get(health))
        .layer(CorsLayer::permissive())
        .with_state(shared)
}

async fn health() -> impl IntoResponse {
    Json(json!({"status": "ok", "name": "RustyERP", "version": "0.3.0"}))
}

// ── Handlers ──

type SharedState = Arc<AppState>;

async fn list_docs(
    State(state): State<SharedState>,
    Path(doctype): Path<String>,
    Query(params): Query<ListParams>,
) -> Result<Json<Value>, AppError> {
    let db = state.db.lock().await;
    let pagination = DocPagination {
        limit: params.limit.unwrap_or(20),
        offset: params.offset.unwrap_or(0),
    };
    let docs = db.get_raw_list(
        &doctype, &[],
        &[DocOrder { field: "modified".into(), descending: true }],
        &pagination,
    )?;
    let count = db.get_raw_count(&doctype)?;
    Ok(Json(json!({"data": docs, "total": count, "limit": pagination.limit, "offset": pagination.offset})))
}

async fn get_doc(
    State(state): State<SharedState>,
    Path((doctype, name)): Path<(String, String)>,
) -> Result<Json<Value>, AppError> {
    let db = state.db.lock().await;
    match db.get_raw_doc(&doctype, &name)? {
        Some(d) => Ok(Json(json!({"data": d}))),
        None => Err(AppError::NotFound(format!("{} '{}' 不存在", doctype, name))),
    }
}

async fn create_doc(
    State(state): State<SharedState>,
    Path(doctype): Path<String>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let db = state.db.lock().await;
    let name = body["name"].as_str().unwrap_or("").to_string();
    let data = body.get("data").cloned().unwrap_or(body);
    db.insert_raw(&name, &doctype, &data)?;
    Ok((StatusCode::CREATED, Json(json!({"data": {"name": name, "doctype": doctype, "docstatus": 0}}))))
}

async fn update_doc(
    State(state): State<SharedState>,
    Path((doctype, name)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, AppError> {
    let db = state.db.lock().await;
    let data = body.get("data").cloned().unwrap_or(body);
    db.save_raw(&name, &doctype, &data)?;
    Ok(Json(json!({"data": {"name": name, "doctype": doctype}})))
}

async fn delete_doc(
    State(state): State<SharedState>,
    Path((doctype, name)): Path<(String, String)>,
) -> Result<Json<Value>, AppError> {
    let db = state.db.lock().await;
    db.delete_raw(&doctype, &name)?;
    Ok(Json(json!({"message": "已删除"})))
}

async fn do_action(
    State(state): State<SharedState>,
    Path((doctype, name)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, AppError> {
    let db = state.db.lock().await;
    let action = body["action"].as_str().unwrap_or("").to_lowercase();
    let new_status: i32 = match action.as_str() {
        "submit" => 1,
        "cancel" => 2,
        _ => return Err(AppError::BadRequest(format!("未知操作: {}", action))),
    };
    let mut doc = db.get_raw_doc(&doctype, &name)?
        .ok_or_else(|| AppError::NotFound(format!("{} '{}' 不存在", doctype, name)))?;
    doc["docstatus"] = json!(new_status);
    db.save_raw(&name, &doctype, &doc)?;
    Ok(Json(json!({"data": {"name": name, "doctype": doctype, "docstatus": new_status, "action": action}})))
}

// ── Query params ──

#[derive(Debug, Deserialize)]
struct ListParams {
    limit: Option<usize>,
    offset: Option<usize>,
}

// ── Error ──

enum AppError {
    NotFound(String),
    BadRequest(String),
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let (status, msg) = match self {
            AppError::NotFound(m) => (StatusCode::NOT_FOUND, m),
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            AppError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (status, Json(json!({"error": msg}))).into_response()
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::Internal(format!("数据库错误: {e}"))
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::Internal(format!("JSON 错误: {e}"))
    }
}
