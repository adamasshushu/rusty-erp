//! 用户认证与授权模块
//! JWT token + bcrypt 密码哈希 + RBAC 角色

use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// JWT 密钥（必须通过环境变量 JWT_SECRET 设置，拒绝默认值）
static JWT_SECRET: LazyLock<String> = LazyLock::new(|| {
    std::env::var("JWT_SECRET").unwrap_or_else(|_| {
        eprintln!("❌ 安全错误: 未设置 JWT_SECRET 环境变量！");
        eprintln!("   生产环境必须设置随机密钥，例如:");
        eprintln!("   export JWT_SECRET=$(openssl rand -hex 32)");
        std::process::exit(1);
    })
});

/// JWT Claims
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    /// 用户 ID
    pub sub: String,
    /// 用户名
    pub username: String,
    /// 角色列表
    pub roles: Vec<String>,
    /// 过期时间 (UNIX timestamp)
    pub exp: usize,
    /// 签发时间
    pub iat: usize,
}

/// 用户角色
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    #[serde(rename = "System Manager")]
    SystemManager,
    #[serde(rename = "User")]
    User,
    #[serde(rename = "Auditor")]
    Auditor,
}

impl Role {
    /// 角色拥有的权限列表
    pub fn permissions(&self) -> Vec<Permission> {
        match self {
            Role::SystemManager => vec![
                Permission::Read,
                Permission::Write,
                Permission::Create,
                Permission::Delete,
                Permission::Submit,
                Permission::Cancel,
                Permission::ManageUsers,
                Permission::SystemConfig,
            ],
            Role::User => vec![
                Permission::Read,
                Permission::Write,
                Permission::Create,
                Permission::Submit,
            ],
            Role::Auditor => vec![Permission::Read],
        }
    }

    pub fn all() -> Vec<Role> {
        vec![Role::SystemManager, Role::User, Role::Auditor]
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Role::SystemManager => write!(f, "System Manager"),
            Role::User => write!(f, "User"),
            Role::Auditor => write!(f, "Auditor"),
        }
    }
}

/// 权限枚举
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Permission {
    Read,
    Write,
    Create,
    Delete,
    Submit,
    Cancel,
    ManageUsers,
    SystemConfig,
}

/// 生成 JWT token
pub fn generate_token(user_id: &str, username: &str, roles: &[String]) -> Result<String, String> {
    let now = Utc::now();
    let exp = now + Duration::hours(24);

    let claims = Claims {
        sub: user_id.to_string(),
        username: username.to_string(),
        roles: roles.to_vec(),
        exp: exp.timestamp() as usize,
        iat: now.timestamp() as usize,
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(JWT_SECRET.as_bytes()),
    )
    .map_err(|e| format!("JWT encoding failed: {}", e))
}

/// 验证并解析 JWT token
pub fn validate_token(token: &str) -> Result<Claims, String> {
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(JWT_SECRET.as_bytes()),
        // 显式指定算法防止算法混淆攻击
        &{
            let mut v = Validation::new(jsonwebtoken::Algorithm::HS256);
            v.validate_exp = true;
            v
        },
    )
    .map(|data| data.claims)
    .map_err(|e| format!("Invalid token: {}", e))
}

/// 从 Bearer header 提取 token
pub fn extract_bearer_token(header: &str) -> Option<&str> {
    header.strip_prefix("Bearer ")
}

/// 哈希密码
pub fn hash_password(password: &str) -> Result<String, String> {
    bcrypt::hash(password, bcrypt::DEFAULT_COST).map_err(|e| format!("Hash error: {}", e))
}

/// 验证密码
pub fn verify_password(password: &str, hash: &str) -> Result<bool, String> {
    bcrypt::verify(password, hash).map_err(|e| format!("Verify error: {}", e))
}

/// 检查用户是否有指定权限
pub fn has_permission(roles: &[String], required: Permission) -> bool {
    roles.iter().any(|r| {
        let role = match r.as_str() {
            "System Manager" => Role::SystemManager,
            "User" => Role::User,
            "Auditor" => Role::Auditor,
            _ => return false,
        };
        role.permissions().contains(&required)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_and_verify() {
        let hash = hash_password("test123").unwrap();
        assert!(verify_password("test123", &hash).unwrap());
        assert!(!verify_password("wrong", &hash).unwrap());
    }

    #[test]
    fn test_jwt_roundtrip() {
        let roles = vec!["System Manager".to_string()];
        let token = generate_token("usr-1", "admin", &roles).unwrap();
        let claims = validate_token(&token).unwrap();
        assert_eq!(claims.sub, "usr-1");
        assert_eq!(claims.username, "admin");
        assert_eq!(claims.roles, roles);
    }

    #[test]
    fn test_has_permission() {
        let admin = vec!["System Manager".to_string()];
        let user = vec!["User".to_string()];
        let auditor = vec!["Auditor".to_string()];

        assert!(has_permission(&admin, Permission::Delete));
        assert!(has_permission(&admin, Permission::ManageUsers));

        assert!(has_permission(&user, Permission::Read));
        assert!(has_permission(&user, Permission::Create));
        assert!(!has_permission(&user, Permission::Delete));
        assert!(!has_permission(&user, Permission::ManageUsers));

        assert!(has_permission(&auditor, Permission::Read));
        assert!(!has_permission(&auditor, Permission::Write));
        assert!(!has_permission(&auditor, Permission::Delete));
    }
}
