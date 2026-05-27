//! Auth 中间件 — Axum Extractor 模式
//! 在 handler 参数中加 `AuthUser` 即可强制鉴权

use axum::{
    extract::FromRequestParts,
    http::{request::Parts, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde_json::json;

use crate::auth::{self, has_permission, Permission};

/// 从 Authorization header 提取并验证的用户信息
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub username: String,
    pub roles: Vec<String>,
}

/// 认证错误响应
fn auth_error(message: &str) -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({"error": message}))).into_response()
}

fn forbidden(message: &str) -> Response {
    (StatusCode::FORBIDDEN, Json(json!({"error": message}))).into_response()
}

/// Axum FromRequestParts 实现 — 自动从 header 提取 JWT 并验证
#[axum::async_trait]
impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let auth_header = parts
            .headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| auth_error("未提供认证令牌"))?;

        let token = auth::extract_bearer_token(auth_header)
            .ok_or_else(|| auth_error("无效的认证格式，请使用 Bearer <token>"))?;

        let claims = auth::validate_token(token).map_err(|e| auth_error(&e))?;

        Ok(AuthUser {
            username: claims.username,
            roles: claims.roles,
        })
    }
}

impl AuthUser {
    /// 检查是否拥有指定角色
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }

    /// 检查是否拥有指定权限
    pub fn can(&self, perm: Permission) -> bool {
        has_permission(&self.roles, perm)
    }

    /// 要求指定角色，否则返回 403
    pub fn require_role(&self, role: &str) -> Result<(), Response> {
        if self.has_role(role) {
            Ok(())
        } else {
            Err(forbidden(&format!("需要 {} 角色", role)))
        }
    }

    /// 要求指定权限，否则返回 403
    pub fn require_permission(&self, perm: Permission) -> Result<(), Response> {
        if self.can(perm) {
            Ok(())
        } else {
            Err(forbidden("权限不足"))
        }
    }
}

/// 需要 System Manager 角色的守卫
pub struct RequireAdmin(pub AuthUser);

#[axum::async_trait]
impl<S> FromRequestParts<S> for RequireAdmin
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if user.has_role("System Manager") {
            Ok(RequireAdmin(user))
        } else {
            Err(forbidden("需要 System Manager 角色"))
        }
    }
}
