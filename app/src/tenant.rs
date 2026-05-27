//! 多租户模块 — tenant 隔离 + 管理

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/tenant/list", get(list_tenants))
        .route("/api/tenant/create", post(create_tenant))
        .route("/api/tenant/:name", get(get_tenant))
}

#[derive(Deserialize)]
struct CreateTenantReq {
    name: String,
    company_name: String,
    admin_email: Option<String>,
    db_name: Option<String>,
}

/// GET /api/tenant/list
async fn list_tenants(
    _auth: crate::middleware::AuthUser,
    State(db): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    if !_auth.can(crate::auth::Permission::SystemConfig) {
        return Err(StatusCode::FORBIDDEN);
    }
    let p = frappe_core::storage::DocPagination { limit: 100, offset: 0 };
    let tenants = db.get_raw_list("Tenant", &[], &[], &p).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data": tenants})))
}

/// POST /api/tenant/create — 创建新租户
async fn create_tenant(
    _auth: crate::middleware::AuthUser,
    State(db): State<AppState>,
    Json(req): Json<CreateTenantReq>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    if !_auth.can(crate::auth::Permission::SystemConfig) {
        return Err(StatusCode::FORBIDDEN);
    }

    // 检查是否已存在
    if db.get_raw_doc("Tenant", &req.name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.is_some() {
        return Ok((StatusCode::CONFLICT, Json(json!({"error": "租户已存在"}))));
    }

    let tenant_data = json!({
        "name": req.name,
        "doctype": "Tenant",
        "company_name": req.company_name,
        "admin_email": req.admin_email.unwrap_or_default(),
        "db_name": req.db_name.unwrap_or_else(|| format!("rusty_erp_{}", req.name)),
        "status": "active",
        "created_on": chrono::Utc::now().to_rfc3339(),
        "docstatus": 0,
    });

    db.insert_raw(&req.name, "Tenant", &tenant_data).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok((StatusCode::CREATED, Json(json!({"message": "租户创建成功", "tenant": req.name}))))
}

/// GET /api/tenant/:name
async fn get_tenant(
    _auth: crate::middleware::AuthUser,
    State(db): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let tenant = db.get_raw_doc("Tenant", &name).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(json!({"data": tenant})))
}
