//! 邮件通知模块 — SMTP 发送 + 审批/预警模板

use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/email/config", get(get_config).post(set_config))
        .route("/api/email/test", post(send_test))
}

#[derive(Deserialize)]
struct EmailConfig {
    smtp_host: String,
    smtp_port: u16,
    smtp_user: String,
    smtp_pass: String,
    from_email: String,
    from_name: Option<String>,
}

/// GET /api/email/config
async fn get_config(
    _auth: crate::middleware::AuthUser,
    State(db): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    if !_auth.can(crate::auth::Permission::SystemConfig) {
        return Err(StatusCode::FORBIDDEN);
    }
    let cfg = db.get_raw_doc("Email Config", "default").await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .unwrap_or_else(|| json!({
            "smtp_host": "", "smtp_port": 587, "smtp_user": "",
            "smtp_pass": "", "from_email": "", "from_name": "RustyERP"
        }));
    Ok(Json(json!({"data": cfg})))
}

/// POST /api/email/config
async fn set_config(
    _auth: crate::middleware::AuthUser,
    State(db): State<AppState>,
    Json(cfg): Json<EmailConfig>,
) -> Result<Json<Value>, StatusCode> {
    if !_auth.can(crate::auth::Permission::SystemConfig) {
        return Err(StatusCode::FORBIDDEN);
    }
    let data = json!({
        "name": "default", "doctype": "Email Config",
        "smtp_host": cfg.smtp_host, "smtp_port": cfg.smtp_port,
        "smtp_user": cfg.smtp_user, "smtp_pass": cfg.smtp_pass,
        "from_email": cfg.from_email, "from_name": cfg.from_name.unwrap_or_default(),
    });
    // upsert
    if db.get_raw_doc("Email Config", "default").await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.is_some() {
        db.save_raw("default", "Email Config", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    } else {
        db.insert_raw("default", "Email Config", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(Json(json!({"message": "邮件配置已保存"})))
}

/// POST /api/email/test — 发送测试邮件
async fn send_test(
    _auth: crate::middleware::AuthUser,
    State(db): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    if !_auth.can(crate::auth::Permission::SystemConfig) {
        return Err(StatusCode::FORBIDDEN);
    }
    let to = body["to"].as_str().unwrap_or("");
    let subject = body["subject"].as_str().unwrap_or("RustyERP 测试邮件");
    let msg_body = body["body"].as_str().unwrap_or("这是一封来自 RustyERP 的测试邮件");

    let cfg = db.get_raw_doc("Email Config", "default").await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    match send_email(&cfg, to, subject, msg_body).await {
        Ok(_) => Ok(Json(json!({"message": "邮件发送成功"}))),
        Err(e) => Ok(Json(json!({"error": format!("发送失败: {}", e)}))),
    }
}

/// 实际发送邮件（使用 lettre）
async fn send_email(cfg: &Value, to: &str, subject: &str, body: &str) -> Result<(), String> {
    let host = cfg["smtp_host"].as_str().unwrap_or("");
    let port = cfg["smtp_port"].as_u64().unwrap_or(587) as u16;
    let user = cfg["smtp_user"].as_str().unwrap_or("");
    let pass = cfg["smtp_pass"].as_str().unwrap_or("");
    let from = cfg["from_email"].as_str().unwrap_or(user);
    let from_name = cfg["from_name"].as_str().unwrap_or("RustyERP");

    if host.is_empty() { return Err("SMTP 未配置".into()); }

    let email = lettre::Message::builder()
        .from(format!("{} <{}>", from_name, from).parse().map_err(|e| format!("{:?}", e))?)
        .to(to.parse().map_err(|e| format!("{:?}", e))?)
        .subject(subject)
        .body(body.to_string())
        .map_err(|e| format!("{:?}", e))?;

    let creds = lettre::transport::smtp::authentication::Credentials::new(user.into(), pass.into());
    let mailer = lettre::SmtpTransport::relay(host)
        .map_err(|e| format!("{:?}", e))?
        .port(port)
        .credentials(creds)
        .build();

    lettre::Transport::send(&mailer, &email).map_err(|e| format!("{:?}", e))?;
    Ok(())
}

/// 发送审批通知
pub async fn notify_approval(db: &AppState, doctype: &str, docname: &str, approver: &str) {
    if let Ok(Some(cfg)) = db.get_raw_doc("Email Config", "default").await {
        let to = format!("{}@company.com", approver); // 简化：通过用户名构造邮箱
        let subject = format!("📋 待审批: {} {}", doctype, docname);
        let body = format!(
            "您有一份待审批单据：\n\n单据类型: {}\n单号: {}\n状态: 待审批\n\n请登录 RustyERP 处理。",
            doctype, docname
        );
        let _ = send_email(&cfg, &to, &subject, &body).await;
    }
}

/// 发送库存预警
pub async fn notify_low_stock(db: &AppState, item_name: &str, qty: f64, safety: f64) {
    if let Ok(Some(cfg)) = db.get_raw_doc("Email Config", "default").await {
        let to = cfg["from_email"].as_str().unwrap_or("admin@company.com");
        let subject = format!("⚠️ 库存预警: {} 低于安全库存", item_name);
        let body = format!(
            "物料 {} 当前库存 {} 低于安全库存 {}，请及时补货。",
            item_name, qty, safety
        );
        let _ = send_email(&cfg, to, &subject, &body).await;
    }
}
