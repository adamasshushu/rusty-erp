//! HR 人事模块 — 员工管理 / 考勤打卡 / 请假审批
//!
//! Doctypes: Employee, Department, Attendance, LeaveRequest
//! 请假状态: pending → approved → rejected

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
        // 员工
        .route("/api/hr/employee", get(employee_list).post(employee_create))
        .route("/api/hr/employee/:name", get(employee_get).put(employee_update))
        // 部门
        .route("/api/hr/department", get(department_list).post(department_create))
        // 考勤
        .route("/api/hr/attendance", get(attendance_list).post(attendance_check_in))
        .route("/api/hr/attendance/:name/check-out", post(attendance_check_out))
        // 请假
        .route("/api/hr/leave", get(leave_list).post(leave_apply))
        .route("/api/hr/leave/:name/approve", post(leave_approve))
        .route("/api/hr/leave/:name/reject", post(leave_reject))
        // 统计
        .route("/api/hr/stats", get(hr_stats))
}

fn ts() -> String { chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string() }
fn td() -> String { chrono::Utc::now().format("%Y-%m-%d").to_string() }

// ═══ 员工 ═══

async fn employee_list(
    State(db): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 500, offset: 0 };
    let order = DocOrder { field: "modified".into(), descending: true };
    let docs = db.get_raw_list("Employee", &[], &[order], &p)
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let total = db.get_raw_count("Employee").await.unwrap_or(0);
    Ok(Json(json!({"data":docs,"total":total})))
}

async fn employee_get(
    State(db): State<AppState>, Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    db.get_raw_doc("Employee", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|d| Json(json!({"data":d})))
        .ok_or(StatusCode::NOT_FOUND)
}

async fn employee_create(
    _auth: AuthUser, State(db): State<AppState>, Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let mut data = body.get("data").cloned().unwrap_or(body);
    let emp_id = db.get_next_series("EMP-.YYYY.-").await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    data["name"] = json!(emp_id);
    data["doctype"] = json!("Employee");
    data["status"] = json!("active");
    data["created"] = json!(ts());
    if data.get("department").is_none() { data["department"] = json!("未分配"); }
    db.insert_raw(&emp_id, "Employee", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"name":emp_id,"doctype":"Employee"}}))))
}

async fn employee_update(
    _auth: AuthUser, State(db): State<AppState>,
    Path(name): Path<String>, Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let data = body.get("data").cloned().unwrap_or(body);
    db.save_raw(&name, "Employee", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":{"name":name}})))
}

// ═══ 部门 ═══

async fn department_list(
    State(db): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 200, offset: 0 };
    let docs = db.get_raw_list("Department", &[], &[], &p)
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    // 统计各部门人数
    let mut depts = Vec::new();
    for d in &docs {
        let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let count = db.get_raw_list("Employee", &[DocFilter::eq("department", name.to_string())], &[], &p)
            .await.unwrap_or_default().len();
        let mut item = d.clone();
        item["employee_count"] = json!(count);
        depts.push(item);
    }
    Ok(Json(json!({"data":depts,"total":depts.len()})))
}

async fn department_create(
    _auth: AuthUser, State(db): State<AppState>, Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let mut data = body.get("data").cloned().unwrap_or(body);
    let name = data["name"].as_str()
        .or(data["department_name"].as_str())
        .unwrap_or("未命名").to_string();
    data["name"] = json!(name);
    data["doctype"] = json!("Department");
    data["created"] = json!(ts());
    db.insert_raw(&name, "Department", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"name":name}}))))
}

// ═══ 考勤 ═══

async fn attendance_list(
    State(db): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 500, offset: 0 };
    let order = DocOrder { field: "check_in".into(), descending: true };
    let docs = db.get_raw_list("Attendance", &[], &[order], &p)
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":docs,"total":docs.len()})))
}

/// 上班打卡
async fn attendance_check_in(
    auth: AuthUser, State(db): State<AppState>, Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let data = body.get("data").cloned().unwrap_or(body);
    let emp_id = data["employee_id"].as_str().unwrap_or(&auth.username);
    let today = td();
    let att_id = format!("ATT-{}-{}", today, emp_id);

    // 检查是否已打卡
    if let Ok(Some(_)) = db.get_raw_doc("Attendance", &att_id).await {
        return Err(StatusCode::CONFLICT);
    }

    let mut att = json!({
        "name": att_id,
        "employee_id": emp_id,
        "employee_name": data.get("employee_name").unwrap_or(&json!("")),
        "check_in": ts(),
        "check_in_ip": data.get("ip").unwrap_or(&json!("")),
        "date": td(),
        "status": "checked_in",
        "doctype": "Attendance",
    });
    db.insert_raw(&att_id, "Attendance", &att).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"name":att_id,"status":"checked_in","check_in":ts()}}))))
}

/// 下班打卡
async fn attendance_check_out(
    _auth: AuthUser, State(db): State<AppState>, Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let mut att = db.get_raw_doc("Attendance", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    att["check_out"] = json!(ts());
    att["status"] = json!("completed");
    db.save_raw(&name, "Attendance", &att).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"message":"已签退","data":{"name":name,"status":"completed"}})))
}

// ═══ 请假 ═══

async fn leave_list(
    State(db): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 500, offset: 0 };
    let order = DocOrder { field: "modified".into(), descending: true };
    let docs = db.get_raw_list("LeaveRequest", &[], &[order], &p)
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":docs,"total":docs.len()})))
}

async fn leave_apply(
    _auth: AuthUser, State(db): State<AppState>, Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let mut data = body.get("data").cloned().unwrap_or(body);
    let leave_id = db.get_next_series("LEAVE-.YYYY.-").await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    data["name"] = json!(leave_id);
    data["doctype"] = json!("LeaveRequest");
    data["status"] = json!("pending");
    data["applied_date"] = json!(td());
    data["created"] = json!(ts());
    db.insert_raw(&leave_id, "LeaveRequest", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"name":leave_id,"status":"pending"}}))))
}

async fn leave_approve(
    _auth: AuthUser, State(db): State<AppState>, Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let mut doc = db.get_raw_doc("LeaveRequest", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    doc["status"] = json!("approved");
    doc["approved_date"] = json!(td());
    db.save_raw(&name, "LeaveRequest", &doc).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"message":"请假已批准","data":{"name":name,"status":"approved"}})))
}

async fn leave_reject(
    _auth: AuthUser, State(db): State<AppState>, Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let mut doc = db.get_raw_doc("LeaveRequest", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    doc["status"] = json!("rejected");
    db.save_raw(&name, "LeaveRequest", &doc).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"message":"请假已驳回","data":{"name":name,"status":"rejected"}})))
}

// ═══ HR 统计 ═══

async fn hr_stats(
    State(db): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 9999, offset: 0 };
    let emp_total = db.get_raw_count("Employee").await.unwrap_or(0);
    let dept_total = db.get_raw_count("Department").await.unwrap_or(0);
    let leaves = db.get_raw_list("LeaveRequest", &[], &[], &p).await.unwrap_or_default();
    let pending_leaves = leaves.iter().filter(|l| l.get("status").and_then(|v| v.as_str()) == Some("pending")).count();
    let today_atts = db.get_raw_list("Attendance", &[DocFilter::eq("date", td())], &[], &p).await.unwrap_or_default().len();

    // 今日出勤率
    let rate = if emp_total > 0 { format!("{:.0}%", (today_atts as f64 / emp_total as f64) * 100.0) } else { "0%".into() };

    Ok(Json(json!({
        "total_employees": emp_total,
        "total_departments": dept_total,
        "today_attendance": today_atts,
        "attendance_rate": rate,
        "pending_leaves": pending_leaves,
        "total_leaves": leaves.len(),
    })))
}
