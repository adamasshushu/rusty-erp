//! 认证 API 端点：注册、登录、用户管理

use axum::{extract::State, http::StatusCode, Json, Router, routing::{get, post}};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth;
use frappe_mysql::MysqlStorage;

pub type App = MysqlStorage;

// ── 请求/响应结构体 ──

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub username: String,
    pub roles: Vec<String>,
    pub expires_in: u64,
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
    pub email: Option<String>,
    #[serde(default = "default_role")]
    pub role: String,
}

fn default_role() -> String {
    "User".into()
}

// ── 路由 ──

pub fn auth_routes() -> Router<App> {
    Router::new()
        .route("/api/auth/register", post(register))
        .route("/api/auth/login", post(login))
        .route("/api/auth/me", get(me))
        .route("/api/auth/users", get(list_users).put(update_user))
        .route("/api/auth/users/:username", axum::routing::delete(delete_user))
}

// ── 处理器 ──

/// POST /api/auth/register — 注册新用户（仅管理员可调用）
async fn register(
    _auth: crate::middleware::AuthUser,
    State(db): State<App>,
    Json(req): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    // 仅 SystemManager 或 Admin 角色可以注册用户
    if !_auth.can(crate::auth::Permission::ManageUsers) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error": "仅管理员可创建用户"})),
        ));
    }
    if req.username.is_empty() || req.password.len() < 6 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "用户名不能为空，密码至少6位"})),
        ));
    }

    let db = db;

    // 检查用户是否已存在
    let existing = db.get_raw_doc("User", &req.username).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;

    if existing.is_some() {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({"error": "用户已存在"})),
        ));
    }

    // 哈希密码
    let password_hash = auth::hash_password(&req.password).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e})))
    })?;

    // 创建用户文档
    let user_data = json!({
        "name": &req.username,
        "doctype": "User",
        "username": &req.username,
        "email": req.email.unwrap_or_default(),
        "password_hash": password_hash,
        "role": req.role,
        "enabled": 1,
        "docstatus": 0,
        "created": chrono::Utc::now().to_rfc3339(),
    });

    db.insert_raw(&req.username, "User", &user_data).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;

    Ok((
        StatusCode::CREATED,
        Json(json!({"message": "注册成功", "username": req.username})),
    ))
}

/// POST /api/auth/login — 登录获取 JWT
async fn login(
    State(db): State<App>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, (StatusCode, Json<Value>)> {
    let db = db;

    // 查找用户
    let user = db.get_raw_doc("User", &req.username).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?.ok_or_else(|| {
        (StatusCode::UNAUTHORIZED, Json(json!({"error": "用户名或密码错误"})))
    })?;

    // 检查是否启用
    if user.get("enabled").and_then(|v| v.as_i64()).unwrap_or(1) == 0 {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error": "账户已被禁用"})),
        ));
    }

    // 验证密码
    let password_hash = user["password_hash"].as_str().unwrap_or("");
    let valid = auth::verify_password(&req.password, password_hash).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e})))
    })?;

    if !valid {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "用户名或密码错误"})),
        ));
    }

    // 获取角色
    let role = user["role"].as_str().unwrap_or("User").to_string();
    let roles = vec![role.clone()];

    // 生成 token
    let token = auth::generate_token(&req.username, &req.username, &roles).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e})))
    })?;

    Ok(Json(LoginResponse {
        token,
        username: req.username,
        roles,
        expires_in: 86400, // 24小时
    }))
}

/// GET /api/auth/me — 获取当前用户信息（需 Bearer token）
async fn me(
    State(db): State<App>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // 提取 token
    let auth_header = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            (StatusCode::UNAUTHORIZED, Json(json!({"error": "未提供认证令牌"})))
        })?;

    let token = auth::extract_bearer_token(auth_header).ok_or_else(|| {
        (StatusCode::UNAUTHORIZED, Json(json!({"error": "无效的认证格式"})))
    })?;

    // 验证 token
    let claims = auth::validate_token(token).map_err(|e| {
        (StatusCode::UNAUTHORIZED, Json(json!({"error": e})))
    })?;

    // 获取用户信息
    let db = db;
    let user = db.get_raw_doc("User", &claims.username).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?.ok_or_else(|| {
        (StatusCode::NOT_FOUND, Json(json!({"error": "用户不存在"})))
    })?;

    Ok(Json(json!({
        "username": claims.username,
        "roles": claims.roles,
        "email": user.get("email").and_then(|v| v.as_str()).unwrap_or(""),
        "enabled": user.get("enabled").and_then(|v| v.as_i64()).unwrap_or(1),
    })))
}

// ── 用户管理 API ──

use crate::middleware::AuthUser;
use crate::auth::Permission;

/// GET /api/auth/users — 列出所有用户
async fn list_users(
    _auth: AuthUser,
    State(db): State<App>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !_auth.can(Permission::ManageUsers) {
        return Err((StatusCode::FORBIDDEN, Json(json!({"error":"无权限"}))));
    }
    let p = frappe_core::storage::DocPagination { limit: 1000, offset: 0 };
    let users = db.get_raw_list("User", &[], &[], &p).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    let result: Vec<Value> = users.iter().map(|u| json!({
        "username": u.get("username").and_then(|v| v.as_str()).unwrap_or(""),
        "email": u.get("email").and_then(|v| v.as_str()).unwrap_or(""),
        "role": u.get("role").and_then(|v| v.as_str()).unwrap_or("User"),
        "enabled": u.get("enabled").and_then(|v| v.as_i64()).unwrap_or(1),
        "created": u.get("created").and_then(|v| v.as_str()).unwrap_or(""),
    })).collect();
    Ok(Json(json!({"data": result})))
}

/// PUT /api/auth/users — 更新用户
#[derive(Deserialize)]
struct UpdateUserReq {
    username: String,
    role: Option<String>,
    enabled: Option<i64>,
    password: Option<String>,
}
async fn update_user(
    _auth: AuthUser,
    State(db): State<App>,
    Json(req): Json<UpdateUserReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !_auth.can(Permission::ManageUsers) {
        return Err((StatusCode::FORBIDDEN, Json(json!({"error":"无权限"}))));
    }
    let mut user = db.get_raw_doc("User", &req.username).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?.ok_or_else(|| {
        (StatusCode::NOT_FOUND, Json(json!({"error": "用户不存在"})))
    })?;
    if let Some(role) = &req.role {
        user["role"] = json!(role);
    }
    if let Some(enabled) = req.enabled {
        user["enabled"] = json!(enabled);
    }
    if let Some(pw) = &req.password {
        if pw.len() >= 6 {
            user["password_hash"] = json!(auth::hash_password(pw).map_err(|e| {
                (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e})))
            })?);
        }
    }
    db.save_raw(&req.username, "User", &user).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    Ok(Json(json!({"message": "更新成功"})))
}

/// DELETE /api/auth/users/:username
async fn delete_user(
    _auth: AuthUser,
    State(db): State<App>,
    axum::extract::Path(username): axum::extract::Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !_auth.can(Permission::ManageUsers) {
        return Err((StatusCode::FORBIDDEN, Json(json!({"error":"无权限"}))));
    }
    if username == "admin" {
        return Err((StatusCode::FORBIDDEN, Json(json!({"error":"不能删除admin"}))));
    }
    db.delete_raw("User", &username).await.map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))
    })?;
    Ok(Json(json!({"message": "已删除"})))
}
