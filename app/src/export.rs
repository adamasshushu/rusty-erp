//! CSV 导出模块 — 所有报表一键导出

use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::collections::HashMap;
use crate::AppState;

#[derive(serde::Deserialize, Default)]
pub struct ExportQuery {
    as_of: Option<String>,
    from: Option<String>,
    to: Option<String>,
    doctype: Option<String>,
}

/// 主导出路由
pub async fn export_handler(
    State(db): State<AppState>,
    axum::extract::Path(report_type): axum::extract::Path<String>,
    Query(q): Query<ExportQuery>,
) -> Result<Response, StatusCode> {
    match report_type.as_str() {
        "trial-balance" => export_trial_balance(&db, q.as_of.as_deref()).await,
        "income-statement" => export_income_statement(&db, q.from.as_deref(), q.to.as_deref()).await,
        "balance-sheet" => export_balance_sheet(&db, q.as_of.as_deref()).await,
        "cash-flow" => export_cash_flow(&db, q.from.as_deref(), q.to.as_deref()).await,
        "aging" => export_aging(&db).await,
        "asset-ledger" => export_asset_ledger(&db).await,
        "inventory" => export_inventory(&db).await,
        "bom" => export_bom(&db).await,
        "work-order" => export_work_order(&db).await,
        "sales" | "purchase" => export_docs(&db, &report_type, q.doctype.as_deref()).await,
        "accounts" => export_accounts(&db).await,
        _ => Err(StatusCode::NOT_FOUND),
    }
}

fn csv_response(filename: &str, csv: String) -> Response {
    let b = format!("{}\n", csv.trim_end());
    ([
        (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
        (header::CONTENT_DISPOSITION, &format!("attachment; filename=\"{}\"; filename*=UTF-8''{}",
            filename, urlencoding(filename))),
    ], b).into_response()
}

fn urlencoding(s: &str) -> String {
    let mut out = String::new();
    for byte in s.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.".contains(&byte) {
            out.push(byte as char);
        } else if byte == b' ' {
            out.push_str("%20");
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

// ── 各报表导出 ──

async fn export_trial_balance(db: &AppState, as_of: Option<&str>) -> Result<Response, StatusCode> {
    let accounts = crate::finance::aggregate_entries(db, as_of, as_of).await?;
    let mut s = String::from("科目编码,借方发生额,贷方发生额,借方余额,贷方余额");
    let mut sorted: Vec<_> = accounts.into_iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (acct, (dr, cr)) in &sorted {
        let net = dr - cr;
        if net.abs() < 1e-4 { continue; }
        let (ndr, ncr) = if net > 0.0 { (net, 0.0_f64) } else { (0.0_f64, -net) };
        s.push_str(&format!("\n{},{:.2},{:.2},{:.2},{:.2}", acct, dr, cr, ndr, ncr));
    }
    Ok(csv_response("试算平衡表.csv", s))
}

async fn export_income_statement(db: &AppState, from: Option<&str>, to: Option<&str>) -> Result<Response, StatusCode> {
    let accounts = crate::finance::aggregate_entries(db, from, to).await?;
    let account_types = load_account_types(db).await?;

    let mut income: Vec<(&String, f64)> = Vec::new();
    let mut expense: Vec<(&String, f64)> = Vec::new();
    for (acct, (dr, cr)) in &accounts {
        let at = account_types.get(acct).map(|s| s.as_str()).unwrap_or("");
        let net = cr - dr;
        match at {
            "income" => { if net.abs() > 1e-4 { income.push((acct, net)); } }
            "expense" => {
                let exp = dr - cr;
                if exp.abs() > 1e-4 { expense.push((acct, exp)); }
            }
            _ => {}
        }
    }

    let mut s = String::from("项目,金额");
    s.push_str("\n一、收入");
    for (acct, amt) in &income { s.push_str(&format!("\n  {},{:.2}", acct, amt)); }
    let tot_inc: f64 = income.iter().map(|(_, a)| a).sum();
    s.push_str(&format!("\n收入合计,{:.2}\n\n二、费用", tot_inc));
    for (acct, amt) in &expense { s.push_str(&format!("\n  {},{:.2}", acct, amt)); }
    let tot_exp: f64 = expense.iter().map(|(_, a)| a).sum();
    s.push_str(&format!("\n费用合计,{:.2}\n\n净利润,{:.2}", tot_exp, tot_inc - tot_exp));
    Ok(csv_response("损益表.csv", s))
}

async fn export_balance_sheet(db: &AppState, as_of: Option<&str>) -> Result<Response, StatusCode> {
    let accounts = crate::finance::aggregate_entries(db, as_of, as_of).await?;
    let account_types = load_account_types(db).await?;

    let mut assets: Vec<(&String, f64)> = Vec::new();
    let mut liab: Vec<(&String, f64)> = Vec::new();
    let mut equity: Vec<(&String, f64)> = Vec::new();
    for (acct, (dr, cr)) in &accounts {
        let at = account_types.get(acct).map(|s| s.as_str()).unwrap_or("");
        let bal = match at {
            "asset" => dr - cr,
            "liability" | "equity" => cr - dr,
            _ => continue,
        };
        if bal.abs() < 1e-4 { continue; }
        match at {
            "asset" => assets.push((acct, bal)),
            "liability" => liab.push((acct, bal)),
            "equity" => equity.push((acct, bal)),
            _ => {}
        }
    }

    let mut s = String::from("项目,金额\n一、资产");
    for (a, v) in &assets { s.push_str(&format!("\n  {},{:.2}", a, v)); }
    let tot_a: f64 = assets.iter().map(|(_, v)| v).sum();
    s.push_str(&format!("\n资产总计,{:.2}\n\n二、负债", tot_a));
    for (a, v) in &liab { s.push_str(&format!("\n  {},{:.2}", a, v)); }
    let tot_l: f64 = liab.iter().map(|(_, v)| v).sum();
    s.push_str(&format!("\n负债合计,{:.2}\n\n三、所有者权益", tot_l));
    for (a, v) in &equity { s.push_str(&format!("\n  {},{:.2}", a, v)); }
    let tot_e: f64 = equity.iter().map(|(_, v)| v).sum();
    s.push_str(&format!("\n权益合计,{:.2}\n\n负债+权益,{:.2}\n平衡,{}", tot_e, tot_l + tot_e, (tot_a - tot_l - tot_e).abs() < 0.01));
    Ok(csv_response("资产负债表.csv", s))
}

async fn export_cash_flow(db: &AppState, from: Option<&str>, to: Option<&str>) -> Result<Response, StatusCode> {
    let accounts = crate::finance::aggregate_entries(db, from, to).await?;
    let account_types = load_account_types(db).await?;

    let mut op_in = 0.0_f64; let mut op_out = 0.0_f64;
    let mut inv_out = 0.0_f64; let mut fin_in = 0.0_f64;
    for (acct, (dr, cr)) in &accounts {
        match account_types.get(acct).map(|s| s.as_str()).unwrap_or("") {
            "income" => op_in += cr - dr,
            "expense" => op_out += dr - cr,
            "asset" => { if acct == "1601" { inv_out += dr - cr; } }
            "equity" => fin_in += cr - dr,
            _ => {}
        }
    }
    let net_op = op_in - op_out;
    let net_inv = -inv_out;
    let net_fin = fin_in;
    let net_chg = net_op + net_inv + net_fin;

    let mut s = String::from("项目,金额\n一、经营活动");
    s.push_str(&format!("\n  销售收入,{:.2}\n  费用支出,{:.2}", op_in, -op_out));
    s.push_str(&format!("\n经营净额,{:.2}\n\n二、投资活动", net_op));
    s.push_str(&format!("\n  购建固定资产,{:.2}\n投资净额,{:.2}", -inv_out, net_inv));
    s.push_str(&format!("\n\n三、筹资活动\n  吸收投资,{:.2}\n筹资净额,{:.2}", fin_in, net_fin));
    s.push_str(&format!("\n\n净增减,{:.2}", net_chg));
    Ok(csv_response("现金流量表.csv", s))
}

async fn export_aging(db: &AppState) -> Result<Response, StatusCode> {
    let p = frappe_core::storage::DocPagination { limit: 5000, offset: 0 };
    let inv = db.get_raw_list("Sales Invoice", &[], &[], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let now = chrono::Utc::now().naive_utc().date();
    let mut buckets = vec![0u32, 0, 0, 0];
    let mut amounts = vec![0.0_f64, 0.0, 0.0, 0.0];
    for doc in &inv {
        let ds = doc.get("docstatus").and_then(|v| v.as_i64()).unwrap_or(0);
        if ds != 1 { continue; }
        let pd = doc.get("posting_date").and_then(|v| v.as_str()).unwrap_or("");
        let total = doc.get("grand_total").or(doc.get("total")).and_then(|v| v.as_f64()).unwrap_or(0.0);
        if let Ok(date) = chrono::NaiveDate::parse_from_str(pd, "%Y-%m-%d") {
            let days = (now - date).num_days();
            let idx = if days <= 30 { 0 } else if days <= 60 { 1 } else if days <= 90 { 2 } else { 3 };
            buckets[idx] += 1;
            amounts[idx] += total;
        }
    }
    let labels = ["0-30天", "31-60天", "61-90天", "90天以上"];
    let mut s = String::from("账龄区间,笔数,金额");
    for i in 0..4 {
        s.push_str(&format!("\n{},{},{:.2}", labels[i], buckets[i], amounts[i]));
    }
    Ok(csv_response("账龄分析.csv", s))
}

// ── 单据导出 ──

async fn export_asset_ledger(db: &AppState) -> Result<Response, StatusCode> {
    let p = frappe_core::storage::DocPagination { limit: 5000, offset: 0 };
    let docs = db.get_raw_list("Asset", &[], &[], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut s = String::from("资产编号,资产名称,类别,购置日期,原值,残值率,月折旧,累计折旧,净值,使用部门,保管人,状态");
    for d in &docs {
        let st = match d.get("status").and_then(|v| v.as_str()).unwrap_or("in_use") {
            "scrapped" => "已报废", "transferred" => "已转移", _ => "使用中"
        };
        s.push_str(&format!("\n{},{},{},{},{:.2},{:.2},{:.2},{:.2},{:.2},{},{},{}",
            d.get("name").and_then(|v| v.as_str()).unwrap_or(""),
            d.get("asset_name").and_then(|v| v.as_str()).unwrap_or(""),
            d.get("asset_category").and_then(|v| v.as_str()).unwrap_or(""),
            d.get("purchase_date").and_then(|v| v.as_str()).unwrap_or(""),
            d.get("purchase_cost").and_then(|v| v.as_f64()).unwrap_or(0.0),
            d.get("salvage_rate").and_then(|v| v.as_f64()).unwrap_or(0.0),
            d.get("monthly_depreciation").and_then(|v| v.as_f64()).unwrap_or(0.0),
            d.get("accumulated_depreciation").and_then(|v| v.as_f64()).unwrap_or(0.0),
            d.get("net_book_value").and_then(|v| v.as_f64()).unwrap_or(0.0),
            d.get("department").and_then(|v| v.as_str()).unwrap_or(""),
            d.get("custodian").and_then(|v| v.as_str()).unwrap_or(""),
            st,
        ));
    }
    Ok(csv_response("资产台账.csv", s))
}

async fn export_inventory(db: &AppState) -> Result<Response, StatusCode> {
    let p = frappe_core::storage::DocPagination { limit: 5000, offset: 0 };
    let docs = db.get_raw_list("Item", &[], &[], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut s = String::from("物料编码,物料名称,规格型号,单位,库存数量,安全库存,标准成本,仓库,状态");
    for d in &docs {
        let status = match d.get("status").and_then(|v| v.as_str()) { Some("inactive")|Some("discontinued") => "停用", _ => "启用" };
        s.push_str(&format!("\n{},{},{},{},{:.0},{:.0},{:.2},{},{}",
            d.get("name").and_then(|v| v.as_str()).unwrap_or(""),
            d.get("item_name").and_then(|v| v.as_str()).unwrap_or(""),
            d.get("specification").and_then(|v| v.as_str()).unwrap_or(""),
            d.get("uom").and_then(|v| v.as_str()).unwrap_or(""),
            d.get("qty_on_hand").and_then(|v| v.as_f64()).unwrap_or(0.0),
            d.get("safety_stock").and_then(|v| v.as_f64()).unwrap_or(0.0),
            d.get("standard_cost").and_then(|v| v.as_f64()).unwrap_or(0.0),
            d.get("warehouse").and_then(|v| v.as_str()).unwrap_or(""),
            status,
        ));
    }
    Ok(csv_response("库存清单.csv", s))
}

async fn export_bom(db: &AppState) -> Result<Response, StatusCode> {
    let p = frappe_core::storage::DocPagination { limit: 5000, offset: 0 };
    let docs = db.get_raw_list("BOM", &[], &[], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut s = String::from("BOM编号,成品物料,版本,数量,状态,材料编码,材料名称,用量,单位");
    for d in &docs {
        let parent = d.get("item").and_then(|v| v.as_str()).unwrap_or("");
        let ver = d.get("version").and_then(|v| v.as_str()).unwrap_or("1");
        let qty = d.get("quantity").and_then(|v| v.as_f64()).unwrap_or(1.0);
        let st = match d.get("docstatus").and_then(|v| v.as_i64()).unwrap_or(0) { 1 => "已提交", _ => "草稿" };
        let items = d.get("items").and_then(|v| v.as_array()).map(|a| a.clone()).unwrap_or_default();
        let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if items.is_empty() {
            s.push_str(&format!("\n{},{},{},{},{},,,,", name, parent, ver, qty, st));
        }
        for item in items {
            s.push_str(&format!("\n{},{},{},{},{},{},{},{:.2},{}",
                name, parent, ver, qty, st,
                item["item_code"].as_str().unwrap_or(""),
                item["item_name"].as_str().unwrap_or(""),
                item["qty"].as_f64().unwrap_or(0.0),
                item["uom"].as_str().unwrap_or(""),
            ));
        }
    }
    Ok(csv_response("BOM清单.csv", s))
}

async fn export_work_order(db: &AppState) -> Result<Response, StatusCode> {
    let p = frappe_core::storage::DocPagination { limit: 5000, offset: 0 };
    let docs = db.get_raw_list("Work Order", &[], &[], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut s = String::from("工单编号,生产物料,计划数量,已产数量,BOM编号,计划开始,计划结束,状态");
    for d in &docs {
        let st = match d.get("status").and_then(|v| v.as_str()).unwrap_or("draft") {
            "draft" => "草稿", "submitted" => "已下达", "in_progress" => "生产中", "completed" => "已完成", _ => "草稿"
        };
        s.push_str(&format!("\n{},{},{:.0},{:.0},{},{},{},{}",
            d.get("name").and_then(|v| v.as_str()).unwrap_or(""),
            d.get("production_item").and_then(|v| v.as_str()).unwrap_or(""),
            d.get("qty_to_produce").and_then(|v| v.as_f64()).unwrap_or(0.0),
            d.get("qty_produced").and_then(|v| v.as_f64()).unwrap_or(0.0),
            d.get("bom").and_then(|v| v.as_str()).unwrap_or(""),
            d.get("planned_start_date").and_then(|v| v.as_str()).unwrap_or(""),
            d.get("planned_end_date").and_then(|v| v.as_str()).unwrap_or(""),
            st,
        ));
    }
    Ok(csv_response("生产工单.csv", s))
}

async fn export_docs(db: &AppState, mod_name: &str, doctype_override: Option<&str>) -> Result<Response, StatusCode> {
    let doctype = doctype_override.unwrap_or(match mod_name {
        "sales" => "Sales Invoice", "purchase" => "Purchase Invoice", _ => "Sales Invoice"
    });
    let p = frappe_core::storage::DocPagination { limit: 5000, offset: 0 };
    let docs = db.get_raw_list(doctype, &[], &[], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut s = String::from("单据编号,客户/供应商,日期,金额,状态");
    for d in &docs {
        let party = d.get("customer").or(d.get("supplier")).and_then(|v| v.as_str()).unwrap_or("");
        let status = match d.get("docstatus").and_then(|v| v.as_i64()).unwrap_or(0) {
            0 => "草稿", 1 => "已提交", 2 => "已取消", _ => "草稿"
        };
        let amount = d.get("grand_total").or(d.get("total")).and_then(|v| v.as_f64()).unwrap_or(0.0);
        s.push_str(&format!("\n{},{},{},{:.2},{}",
            d.get("name").and_then(|v| v.as_str()).unwrap_or(""),
            party,
            d.get("posting_date").and_then(|v| v.as_str()).unwrap_or(""),
            amount, status,
        ));
    }
    Ok(csv_response(&format!("{}.csv", mod_name), s))
}

async fn export_accounts(db: &AppState) -> Result<Response, StatusCode> {
    let p = frappe_core::storage::DocPagination { limit: 500, offset: 0 };
    let docs = db.get_raw_list("Account", &[], &[], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut s = String::from("科目编码,科目名称,类型,上级科目,余额方向");
    for d in &docs {
        let at = match d.get("account_type").and_then(|v| v.as_str()).unwrap_or("") {
            "asset" => "资产", "liability" => "负债", "equity" => "权益", "income" => "收入", "expense" => "费用", x => x,
        };
        s.push_str(&format!("\n{},{},{},{},{}",
            d.get("name").and_then(|v| v.as_str()).unwrap_or(""),
            d.get("account_name").and_then(|v| v.as_str()).unwrap_or(""),
            at,
            d.get("parent_account").and_then(|v| v.as_str()).unwrap_or(""),
            d.get("balance_direction").and_then(|v| v.as_str()).unwrap_or(""),
        ));
    }
    Ok(csv_response("会计科目.csv", s))
}

// ── Helpers ──

async fn load_account_types(db: &AppState) -> Result<HashMap<String, String>, StatusCode> {
    let p = frappe_core::storage::DocPagination { limit: 500, offset: 0 };
    let docs = db.get_raw_list("Account", &[], &[], &p).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut map = HashMap::new();
    for a in &docs {
        if let (Some(code), Some(typ)) = (a["name"].as_str(), a["account_type"].as_str()) {
            map.insert(code.to_string(), typ.to_string());
        }
    }
    Ok(map)
}
