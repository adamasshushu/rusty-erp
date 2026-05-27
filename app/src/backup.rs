//! 数据备份模块 — mysqldump + API 下载/恢复

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;
use std::process::Command;

use crate::AppState;

/// 从环境变量读取数据库凭据（避免硬编码密码）
fn db_creds() -> (String, String) {
    let user = std::env::var("DB_USER").unwrap_or_else(|_| "rusty".into());
    let pass = std::env::var("DB_PASSWORD").unwrap_or_else(|_| {
        eprintln!("⚠️  DB_PASSWORD 未设置，备份功能可能失败");
        String::new()
    });
    (user, pass)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/backup/download", get(download_backup))
        .route("/api/backup/restore", get(restore_confirm))
        .route("/api/backup/restore/:filename", axum::routing::post(restore_backup))
        .route("/api/backup/status", get(backup_status))
}

/// GET /api/backup/download — 导出 SQL 备份文件
async fn download_backup(
    _auth: crate::middleware::AuthUser,
) -> Result<Response, StatusCode> {
    if !_auth.can(crate::auth::Permission::SystemConfig) {
        return Err(StatusCode::FORBIDDEN);
    }

    let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let filename = format!("/tmp/rusty_erp_backup_{}.sql", ts);
    let (db_user, db_pass) = db_creds();

    let output = Command::new("mysqldump")
        .args(["-u", &db_user, format!("-p{}", db_pass).as_str(), "--single-transaction", "--routines", "--triggers", "rusty_erp"])
        .arg("--result-file").arg(&filename)
        .output()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !output.status.success() {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    let content = tokio::fs::read_to_string(&filename)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // 删除临时文件
    let _ = tokio::fs::remove_file(&filename).await;

    Ok(([
        (header::CONTENT_TYPE, "application/sql; charset=utf-8"),
        (header::CONTENT_DISPOSITION, &format!("attachment; filename=\"rusty_erp_{}.sql\"", ts)),
    ], content).into_response())
}

/// GET /api/backup/status — 查看最近备份和数据库状态
async fn backup_status(
    State(db): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // 统计各 doctype 数量
    let doctypes = ["Sales Invoice","Purchase Invoice","Asset","Journal Entry","Item","BOM","Work Order","User","Account"];
    let mut counts = vec![];
    for dt in &doctypes {
        let count = db.get_raw_count(dt).await.unwrap_or(0);
        counts.push(json!({"doctype": dt, "count": count}));
    }

    // 数据库大小
    let (db_user, db_pass) = db_creds();
    let size_output = Command::new("mysql")
        .args(["-u", &db_user, format!("-p{}", db_pass).as_str(), "-e", "SELECT ROUND(SUM(data_length+index_length)/1024/1024,2) AS size_mb FROM information_schema.tables WHERE table_schema='rusty_erp'"])
        .output();
    let size_mb = size_output
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.lines().nth(1).map(|l| l.to_string()))
        .unwrap_or_default();

    Ok(Json(json!({
        "database": "rusty_erp",
        "size_mb": size_mb.trim(),
        "tables": counts,
    })))
}

/// GET /api/backup/restore — 确认可恢复的备份列表
async fn restore_confirm(
    _auth: crate::middleware::AuthUser,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !_auth.can(crate::auth::Permission::SystemConfig) {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(Json(json!({
        "message": "安全警告: 恢复备份将覆盖当前数据！请通过 POST /api/backup/restore/:filename 并上传文件来执行恢复"
    })))
}

/// POST /api/backup/restore/:filename — 恢复数据库
async fn restore_backup(
    _auth: crate::middleware::AuthUser,
    axum::extract::Path(filename): axum::extract::Path<String>,
    body: String,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !_auth.can(crate::auth::Permission::SystemConfig) {
        return Err(StatusCode::FORBIDDEN);
    }
    if !filename.ends_with(".sql") || filename.contains('/') || filename.contains('\\') || filename.contains("..") {
        return Err(StatusCode::BAD_REQUEST);
    }

    let tmp = format!("/tmp/{}", filename);
    tokio::fs::write(&tmp, &body).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let (db_user, db_pass) = db_creds();
    // 使用管道方式恢复，避免命令注入
    let output = Command::new("mysql")
        .args(["-u", &db_user, format!("-p{}", db_pass).as_str(), "rusty_erp"])
        .stdin(std::process::Stdio::from(
            std::fs::File::open(&tmp).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        ))
        .output()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let _ = tokio::fs::remove_file(&tmp).await;

    if output.status.success() {
        Ok(Json(json!({"message": "数据库恢复成功"})))
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        Ok(Json(json!({"error": "恢复失败", "detail": err.to_string()})))
    }
}
