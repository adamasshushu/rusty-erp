//! CRM 模块 — 客户关系管理
//!
//! 包含：客户(Customer)、联系人(Contact)、线索(Lead)、商机(Opportunity)
//! 线索状态：new → contacted → qualified → converted → lost
//! 商机阶段：prospecting → qualification → proposal → negotiation → won → lost

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use crate::middleware::AuthUser;
use crate::AppState;
use frappe_core::storage::{DocFilter, DocOrder, DocPagination};
use serde_json::{json, Value};

/// CRM 路由汇总
pub fn routes() -> Router<AppState> {
    Router::new()
        // 客户 — 支持复数 + 单数路径
        .route("/api/crm/customers", get(customer_list).post(customer_create))
        .route("/api/crm/customer", get(customer_list).post(customer_create))
        .route("/api/crm/customers/:name", get(customer_get).put(customer_update))
        .route("/api/crm/customer/:name", get(customer_get).put(customer_update))
        .route("/api/crm/customers/:name/contacts", get(contact_list).post(contact_create))
        .route("/api/crm/customers/:name/contacts/:cid", put(contact_update))

        // 线索 — 支持复数 + 单数路径
        .route("/api/crm/leads", get(lead_list).post(lead_create))
        .route("/api/crm/lead", get(lead_list).post(lead_create))
        .route("/api/crm/leads/:name", get(lead_get).put(lead_update))
        .route("/api/crm/lead/:name", get(lead_get).put(lead_update))
        .route("/api/crm/leads/:name/convert", post(lead_convert))
        .route("/api/crm/lead/:name/convert", post(lead_convert))

        // 商机 — 支持复数 + 单数路径
        .route("/api/crm/opportunities", get(opportunity_list).post(opportunity_create))
        .route("/api/crm/opportunity", get(opportunity_list).post(opportunity_create))
        .route("/api/crm/opportunities/:name", get(opportunity_get).put(opportunity_update))
        .route("/api/crm/opportunity/:name", get(opportunity_get).put(opportunity_update))

        // 联系人
        .route("/api/crm/contact", get(contact_list_all).post(contact_create_standalone))
        .route("/api/crm/contact/:name", get(contact_get_standalone).put(contact_update_standalone))

        // 销售管道
        .route("/api/crm/pipeline", get(pipeline))

        // CRM 统计（仪表盘用）
        .route("/api/crm/stats", get(crm_stats))
}

// ── 客户 ──

#[derive(serde::Deserialize)]
struct CrmQ {
    limit: Option<usize>,
    offset: Option<usize>,
    search: Option<String>,
}

async fn customer_list(
    State(db): State<AppState>,
    Query(q): Query<CrmQ>,
) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: q.limit.unwrap_or(100), offset: q.offset.unwrap_or(0) };
    let order = DocOrder { field: "modified".into(), descending: true };
    let filters: Vec<DocFilter> = vec![];
    let (docs, total) = if let Some(ref search) = q.search {
        if search.is_empty() {
            (db.get_raw_list("Customer", &filters, &[order], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
             db.get_raw_count("Customer").await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?)
        } else {
            (db.search_raw("Customer", search, &[order], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
             db.get_raw_count_filtered("Customer", search).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?)
        }
    } else {
        (db.get_raw_list("Customer", &filters, &[order], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
         db.get_raw_count("Customer").await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?)
    };
    Ok(Json(json!({"data":docs,"total":total})))
}

async fn customer_get(
    State(db): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let doc = db.get_raw_doc("Customer", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    // 携带联系人
    let cp = DocPagination { limit: 200, offset: 0 };
    let contacts = db.get_raw_list("Contact", &[DocFilter::eq("customer", name.clone())], &[], &cp)
        .await.unwrap_or_default();
    Ok(Json(json!({"data":doc,"contacts":contacts})))
}

async fn customer_create(
    _auth: AuthUser, State(db): State<AppState>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let name = body["name"].as_str().unwrap_or("").to_string();
    let mut data = body.get("data").cloned().unwrap_or(body);
    if data.get("name").is_none() { data["name"] = json!(name); }
    data["doctype"] = json!("Customer");
    db.insert_raw(&name, "Customer", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"name":name,"doctype":"Customer"}}))))
}

async fn customer_update(
    _auth: AuthUser, State(db): State<AppState>,
    Path(name): Path<String>, Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let data = body.get("data").cloned().unwrap_or(body);
    db.save_raw(&name, "Customer", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":{"name":name,"doctype":"Customer"}})))
}

// ── 联系人 ──

async fn contact_list(
    State(db): State<AppState>,
    Path(customer): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 200, offset: 0 };
    let contacts = db.get_raw_list("Contact", &[DocFilter::eq("customer", customer)], &[], &p)
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":contacts})))
}

async fn contact_create(
    _auth: AuthUser, State(db): State<AppState>,
    Path(customer): Path<String>, Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let name = body["name"].as_str().unwrap_or("").to_string();
    let mut data = body.get("data").cloned().unwrap_or(body);
    data["customer"] = json!(customer);
    data["doctype"] = json!("Contact");
    if data.get("name").is_none() { data["name"] = json!(name); }
    db.insert_raw(&name, "Contact", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"name":name}}))))
}

async fn contact_update(
    _auth: AuthUser, State(db): State<AppState>,
    Path((_customer, cid)): Path<(String, String)>, Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let data = body.get("data").cloned().unwrap_or(body);
    db.save_raw(&cid, "Contact", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":{"name":cid}})))
}

// ── 线索 ──

async fn lead_list(
    State(db): State<AppState>,
    Query(q): Query<CrmQ>,
) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: q.limit.unwrap_or(100), offset: q.offset.unwrap_or(0) };
    let order = DocOrder { field: "modified".into(), descending: true };
    let docs = db.get_raw_list("Lead", &[], &[order], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let total = db.get_raw_count("Lead").await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":docs,"total":total})))
}

async fn lead_get(
    State(db): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let doc = db.get_raw_doc("Lead", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(json!({"data":doc})))
}

async fn lead_create(
    _auth: AuthUser, State(db): State<AppState>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let name = body["name"].as_str().unwrap_or("").to_string();
    let mut data = body.get("data").cloned().unwrap_or(body);
    if data.get("name").is_none() { data["name"] = json!(name); }
    if data.get("status").is_none() { data["status"] = json!("new"); }
    data["doctype"] = json!("Lead");
    db.insert_raw(&name, "Lead", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"name":name,"doctype":"Lead"}}))))
}

async fn lead_update(
    _auth: AuthUser, State(db): State<AppState>,
    Path(name): Path<String>, Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let data = body.get("data").cloned().unwrap_or(body);
    db.save_raw(&name, "Lead", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":{"name":name}})))
}

/// 线索转客户 + 商机
async fn lead_convert(
    _auth: AuthUser, State(db): State<AppState>,
    Path(name): Path<String>, Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let lead = db.get_raw_doc("Lead", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let company = body["company_name"].as_str()
        .or_else(|| lead.get("company_name").and_then(|v| v.as_str()))
        .unwrap_or("未命名客户");

    // 创建客户 (insert_raw for new record)
    let cust = json!({
        "name": company,
        "company_name": company,
        "phone": lead.get("phone").unwrap_or(&json!("")),
        "email": lead.get("email").unwrap_or(&json!("")),
        "source": lead.get("source").unwrap_or(&json!("")),
        "doctype": "Customer",
    });
    db.insert_raw(company, "Customer", &cust).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // 创建商机 (insert_raw for new record)
    let opp_name = format!("{}-opp", company);
    let expected_value = body["expected_value"].as_f64()
        .or_else(|| lead.get("expected_value").and_then(|v| v.as_f64()))
        .unwrap_or(0.0);
    let opp = json!({
        "name": opp_name,
        "customer": company,
        "opportunity_name": company.to_string(),
        "stage": "lead",
        "expected_value": expected_value,
        "expected_close_date": body.get("expected_close_date").unwrap_or(&json!("")),
        "doctype": "Opportunity",
    });
    db.insert_raw(&opp_name, "Opportunity", &opp).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // 标记线索为已转换
    let mut updated_lead = lead.clone();
    updated_lead["status"] = json!("converted");
    updated_lead["converted_customer"] = json!(company);
    updated_lead["converted_opportunity"] = json!(opp_name);
    updated_lead["converted_date"] = json!(chrono::Utc::now().format("%Y-%m-%d").to_string());
    db.save_raw(&name, "Lead", &updated_lead).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(json!({
        "message": "线索已转换为客户和商机",
        "customer": company,
        "opportunity": opp_name,
    })))
}

// ── 商机 ──

async fn opportunity_list(
    State(db): State<AppState>,
    Query(q): Query<CrmQ>,
) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: q.limit.unwrap_or(100), offset: q.offset.unwrap_or(0) };
    let order = DocOrder { field: "modified".into(), descending: true };
    let docs = db.get_raw_list("Opportunity", &[], &[order], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let total = db.get_raw_count("Opportunity").await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":docs,"total":total})))
}

async fn opportunity_get(
    State(db): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let doc = db.get_raw_doc("Opportunity", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(json!({"data":doc})))
}

async fn opportunity_create(
    _auth: AuthUser, State(db): State<AppState>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let name = body["name"].as_str().unwrap_or("").to_string();
    let mut data = body.get("data").cloned().unwrap_or(body);
    if data.get("name").is_none() { data["name"] = json!(name); }
    if data.get("stage").is_none() { data["stage"] = json!("prospecting"); }
    data["doctype"] = json!("Opportunity");
    db.insert_raw(&name, "Opportunity", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"name":name,"doctype":"Opportunity"}}))))
}

async fn opportunity_update(
    _auth: AuthUser, State(db): State<AppState>,
    Path(name): Path<String>, Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let data = body.get("data").cloned().unwrap_or(body);
    db.save_raw(&name, "Opportunity", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":{"name":name}})))
}

// ── CRM 统计 ──

async fn crm_stats(
    State(db): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 9999, offset: 0 };

    // 客户总数
    let total_customers = db.get_raw_count("Customer").await.unwrap_or(0);

    // 线索按状态统计
    let leads = db.get_raw_list("Lead", &[], &[], &p).await.unwrap_or_default();
    let mut lead_statuses = serde_json::Map::new();
    for l in &leads {
        let s = l.get("status").and_then(|v| v.as_str()).unwrap_or("new").to_string();
        *lead_statuses.entry(s).or_insert(json!(0)) = json!(lead_statuses.get(&s).and_then(|v| v.as_i64()).unwrap_or(0) + 1);
    }

    // 商机按阶段统计 + 预期金额
    let opps = db.get_raw_list("Opportunity", &[], &[], &p).await.unwrap_or_default();
    let mut opp_stages = serde_json::Map::new();
    let mut pipeline_value = 0.0_f64;
    for o in &opps {
        let stage = o.get("stage").and_then(|v| v.as_str()).unwrap_or("prospecting").to_string();
        *opp_stages.entry(stage).or_insert(json!(0)) = json!(opp_stages.get(&stage).and_then(|v| v.as_i64()).unwrap_or(0) + 1);
        if let Some(v) = o.get("expected_value").and_then(|v| v.as_f64()) {
            pipeline_value += v;
        }
    }

    // 本周新建线索
    let today = chrono::Utc::now();
    let week_ago = today - chrono::Duration::days(7);
    let week_str = week_ago.format("%Y-%m-%d").to_string();
    let recent_leads = db.search_raw("Lead", &week_str, &[], &p)
        .await.unwrap_or_default()
        .len();

    Ok(Json(json!({
        "total_customers": total_customers,
        "total_leads": leads.len(),
        "lead_statuses": lead_statuses,
        "total_opportunities": opps.len(),
        "opportunity_stages": opp_stages,
        "pipeline_value": format!("{:.2}", pipeline_value),
        "recent_leads_7d": recent_leads,
    })))
}

// ── 联系人独立端点（不绑定客户）──

async fn contact_list_all(
    State(db): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 500, offset: 0 };
    let docs = db.get_raw_list("Contact", &[], &[], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let total = db.get_raw_count("Contact").await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":docs,"total":total})))
}

async fn contact_create_standalone(
    _auth: AuthUser, State(db): State<AppState>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let mut data = body.get("data").cloned().unwrap_or(body);
    let name = data["name"].as_str().or(data["full_name"].as_str()).unwrap_or("").to_string();
    data["name"] = json!(name);
    data["doctype"] = json!("Contact");
    db.insert_raw(&name, "Contact", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(json!({"data":{"name":name}}))))
}

async fn contact_get_standalone(
    State(db): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let doc = db.get_raw_doc("Contact", &name).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(json!({"data":doc})))
}

async fn contact_update_standalone(
    _auth: AuthUser, State(db): State<AppState>,
    Path(name): Path<String>, Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let data = body.get("data").cloned().unwrap_or(body);
    db.save_raw(&name, "Contact", &data).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"data":{"name":name}})))
}

// ── 销售管道看板 ──

async fn pipeline(
    State(db): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    let p = DocPagination { limit: 500, offset: 0 };
    let opps = db.get_raw_list("Opportunity", &[], &[], &p)
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let stages_order = ["lead", "qualified", "proposal", "negotiation", "closed_won", "closed_lost"];
    let mut pipeline: Vec<Value> = stages_order.iter().map(|s| {
        let items: Vec<&Value> = opps.iter()
            .filter(|o| o.get("stage").and_then(|v| v.as_str()) == Some(s))
            .collect();
        let total: f64 = items.iter()
            .filter_map(|o| o.get("expected_value").or(o.get("expected_amount")).and_then(|v| v.as_f64()))
            .sum();
        json!({
            "stage": s,
            "label": match *s {
                "lead" => "线索", "qualified" => "合格", "proposal" => "提案",
                "negotiation" => "谈判", "closed_won" => "已赢单", "closed_lost" => "已丢单",
                _ => s,
            },
            "count": items.len(),
            "amount": total,
            "items": items.iter().map(|o| json!({
                "name": o.get("name").unwrap_or(&json!("")),
                "title": o.get("opportunity_name").or(o.get("title")).unwrap_or(&json!("")),
                "customer_name": o.get("customer").unwrap_or(&json!("")),
                "expected_amount": o.get("expected_value").or(o.get("expected_amount")).unwrap_or(&json!(0)),
                "probability": o.get("probability").unwrap_or(&json!(0)),
                "expected_close_date": o.get("expected_close_date").unwrap_or(&json!("")),
            })).collect::<Vec<_>>(),
        })
    }).collect();

    Ok(Json(json!({"pipeline": pipeline})))
}
