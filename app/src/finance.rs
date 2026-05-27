//! 财务报表模块 — 试算平衡表 · 损益表 · 资产负债表
//!
//! 基于日记账分录行实时汇总，无需额外数据表。

use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use chrono::NaiveDate;
use serde_json::{json, Value};
use std::collections::HashMap;
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/finance/trial-balance", get(trial_balance))
        .route("/api/finance/income-statement", get(income_statement))
        .route("/api/finance/balance-sheet", get(balance_sheet))
        .route("/api/finance/cash-flow", get(cash_flow))
        .route("/api/finance/aging", get(aging_analysis))
}

#[derive(serde::Deserialize, Default)]
struct FinQuery {
    as_of: Option<String>,
    from: Option<String>,
    to: Option<String>,
}

/// 从日记账 doc 的 lines JSON 字符串中提取分录行
fn parse_journal_lines(doc: &Value) -> Vec<JournalLine> {
    let lines_str = doc.get("lines").and_then(|v| v.as_str()).unwrap_or("[]");
    serde_json::from_str::<Vec<JournalLine>>(lines_str).unwrap_or_default()
}

#[derive(serde::Deserialize, Debug, Clone)]
struct JournalLine {
    account: String,
    debit: f64,
    credit: f64,
    #[allow(dead_code)]
    description: Option<String>,
}

/// 汇总所有日记账分录 → 按科目分组 {account: {debit, credit}}
pub async fn aggregate_entries(
    db: &AppState,
    from_date: Option<&str>,
    to_date: Option<&str>,
) -> Result<HashMap<String, (f64, f64)>, StatusCode> {
    let p = frappe_core::storage::DocPagination { limit: 5000, offset: 0 };
    let order = frappe_core::storage::DocOrder { field: "posting_date".into(), descending: false };
    let entries = db.get_raw_list("Journal Entry", &[], &[order], &p)
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut accounts: HashMap<String, (f64, f64)> = HashMap::new();

    for entry in &entries {
        // 日期过滤
        if let (Some(from), Some(to)) = (from_date, to_date) {
            let posting = entry.get("posting_date").and_then(|v| v.as_str()).unwrap_or("");
            if posting < from || posting > to { continue; }
        } else if let Some(as_of) = from_date.or(to_date) {
            let posting = entry.get("posting_date").and_then(|v| v.as_str()).unwrap_or("");
            if posting > as_of { continue; }
        }

        let docstatus = entry.get("docstatus").and_then(|v| v.as_i64()).unwrap_or(0);
        if docstatus != 1 && docstatus != 0 { continue; } // skip cancelled

        for line in parse_journal_lines(entry) {
            let (mut dr, mut cr) = accounts.get(&line.account).copied().unwrap_or((0.0, 0.0));
            dr += line.debit;
            cr += line.credit;
            accounts.insert(line.account, (dr, cr));
        }
    }

    Ok(accounts)
}

/// 试算平衡表 GET /api/finance/trial-balance?as_of=2026-05-17
async fn trial_balance(
    State(db): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<FinQuery>,
) -> Result<Json<Value>, StatusCode> {
    let as_of = q.as_of.as_deref();
    let accounts = aggregate_entries(&db, as_of, as_of).await?;

    let mut total_dr = 0.0_f64;
    let mut total_cr = 0.0_f64;
    let mut rows: Vec<Value> = Vec::new();

    // 按科目编码排序
    let mut sorted: Vec<(String, (f64, f64))> = accounts.into_iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    for (acct, (dr, cr)) in &sorted {
        let net = dr - cr;
        if net.abs() < 1e-4 { continue; } // 余额为零跳过
        let (net_dr, net_cr) = if net > 0.0 { (net, 0.0_f64) } else { (0.0_f64, -net) };
        total_dr += net_dr;
        total_cr += net_cr;
        rows.push(json!({
            "account": acct,
            "debit_total": dr,
            "credit_total": cr,
            "balance_debit": net_dr,
            "balance_credit": net_cr,
        }));
    }

    Ok(Json(json!({
        "as_of": as_of.unwrap_or("all"),
        "rows": rows,
        "total_debit": total_dr,
        "total_credit": total_cr,
        "balanced": (total_dr - total_cr).abs() < 0.01,
    })))
}

/// 损益表 GET /api/finance/income-statement?from=2026-01-01&to=2026-05-17
async fn income_statement(
    State(db): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<FinQuery>,
) -> Result<Json<Value>, StatusCode> {
    let accounts = aggregate_entries(&db, q.from.as_deref(), q.to.as_deref()).await?;

    // 获取科目列表以确定类型
    let all_accounts = db.get_raw_list("Account", &[], &[], &frappe_core::storage::DocPagination{limit:500,offset:0})
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let account_types: HashMap<String, String> = all_accounts.iter()
        .filter_map(|a| {
            let code = a.get("account_code").or(a.get("name")).and_then(|v| v.as_str())?;
            let atype = a.get("account_type").and_then(|v| v.as_str())?;
            Some((code.to_string(), atype.to_string()))
        })
        .collect();

    let mut income_items: Vec<Value> = Vec::new();
    let mut expense_items: Vec<Value> = Vec::new();
    let mut total_income = 0.0_f64;
    let mut total_expense = 0.0_f64;

    for (acct, (dr, cr)) in &accounts {
        let atype = account_types.get(acct).map(|s| s.as_str()).unwrap_or("");
        let net = cr - dr; // 收入=贷方-借方, 费用=借方-贷方
        match atype {
            "income" => {
                if net.abs() > 1e-4 {
                    total_income += net;
                    income_items.push(json!({"account": acct, "amount": net}));
                }
            }
            "expense" => {
                let exp = dr - cr; // 费用 = 借方 - 贷方
                if exp.abs() > 1e-4 {
                    total_expense += exp;
                    expense_items.push(json!({"account": acct, "amount": exp}));
                }
            }
            _ => {}
        }
    }

    let net_profit = total_income - total_expense;

    Ok(Json(json!({
        "period_from": q.from.as_deref().unwrap_or("beginning"),
        "period_to": q.to.as_deref().unwrap_or("now"),
        "income": {"items": income_items, "total": total_income},
        "expense": {"items": expense_items, "total": total_expense},
        "net_profit": net_profit,
    })))
}

/// 资产负债表 GET /api/finance/balance-sheet?as_of=2026-05-17
async fn balance_sheet(
    State(db): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<FinQuery>,
) -> Result<Json<Value>, StatusCode> {
    let accounts = aggregate_entries(&db, q.as_of.as_deref(), q.as_of.as_deref()).await?;

    let all_accounts = db.get_raw_list("Account", &[], &[], &frappe_core::storage::DocPagination{limit:500,offset:0})
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let account_types: HashMap<String, String> = all_accounts.iter()
        .filter_map(|a| {
            let code = a.get("account_code").or(a.get("name")).and_then(|v| v.as_str())?;
            let atype = a.get("account_type").and_then(|v| v.as_str())?;
            Some((code.to_string(), atype.to_string()))
        })
        .collect();

    let mut assets: Vec<Value> = Vec::new();
    let mut liabilities: Vec<Value> = Vec::new();
    let mut equity: Vec<Value> = Vec::new();
    let mut total_assets = 0.0_f64;
    let mut total_liabilities = 0.0_f64;
    let mut total_equity = 0.0_f64;

    for (acct, (dr, cr)) in accounts.iter() {
        let atype = account_types.get(acct).map(|s| s.as_str()).unwrap_or("");
        let balance = dr - cr; // 资产 = 借方-贷方 >0, 负债/权益 = 贷方-借方 >0
        let display_balance = match atype {
            "asset" => balance,
            "liability" | "equity" => cr - dr,
            _ => continue,
        };
        if display_balance.abs() < 1e-4 { continue; }
        match atype {
            "asset" => {
                total_assets += display_balance;
                assets.push(json!({"account": acct, "amount": display_balance}));
            }
            "liability" => {
                total_liabilities += display_balance;
                liabilities.push(json!({"account": acct, "amount": display_balance}));
            }
            "equity" => {
                total_equity += display_balance;
                equity.push(json!({"account": acct, "amount": display_balance}));
            }
            _ => {}
        }
    }

    // 计算本期净利润（收入-费用），计入留存收益
    let mut net_profit = 0.0_f64;
    for (acct, (dr, cr)) in accounts.iter() {
        let atype = account_types.get(acct).map(|s| s.as_str()).unwrap_or("");
        match atype {
            "income" => net_profit += cr - dr,
            "expense" => net_profit -= dr - cr,
            _ => {}
        }
    }

    // 将净利润加入权益
    if net_profit.abs() > 1e-4 {
        total_equity += net_profit;
        equity.push(json!({"account": "3301", "amount": net_profit, "label": "本期利润"}));
    }

    Ok(Json(json!({
        "as_of": q.as_of.as_deref().unwrap_or("now"),
        "assets": {"items": assets, "total": total_assets},
        "liabilities": {"items": liabilities, "total": total_liabilities},
        "equity": {"items": equity, "total": total_equity},
        "total_liabilities_and_equity": total_liabilities + total_equity,
        "balanced": (total_assets - (total_liabilities + total_equity)).abs() < 0.01,
    })))
}

/// 现金流量表 GET /api/finance/cash-flow?from=2026-01-01&to=2026-05-17
async fn cash_flow(
    State(db): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<FinQuery>,
) -> Result<Json<Value>, StatusCode> {
    let accounts = aggregate_entries(&db, q.from.as_deref(), q.to.as_deref()).await?;

    let all_accounts = db.get_raw_list("Account", &[], &[], &frappe_core::storage::DocPagination{limit:500,offset:0})
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let account_types: HashMap<String, String> = all_accounts.iter()
        .filter_map(|a| {
            let code = a.get("account_code").or(a.get("name")).and_then(|v| v.as_str())?;
            let atype = a.get("account_type").and_then(|v| v.as_str())?;
            Some((code.to_string(), atype.to_string()))
        }).collect();

    // 经营活动: 收入 - 费用
    let mut operating_inflow = 0.0_f64;
    let mut operating_outflow = 0.0_f64;
    for (acct, (dr, cr)) in &accounts {
        match account_types.get(acct).map(|s| s.as_str()).unwrap_or("") {
            "income" => operating_inflow += cr - dr,
            "expense" => operating_outflow += dr - cr,
            _ => {}
        }
    }

    // 投资活动: 固定资产变动
    let mut investing_outflow = 0.0_f64;
    for (acct, (dr, cr)) in accounts.iter() {
        if account_types.get(acct).map(|s| s.as_str()).unwrap_or("") == "asset" {
            if acct == "1601" { // 固定资产
                investing_outflow += dr - cr;
            }
        }
    }

    // 筹资活动: 权益变动
    let mut financing_inflow = 0.0_f64;
    for (acct, (dr, cr)) in accounts.iter() {
        if account_types.get(acct).map(|s| s.as_str()).unwrap_or("") == "equity" {
            financing_inflow += cr - dr;
        }
    }

    let net_operating = operating_inflow - operating_outflow;
    let net_investing = -investing_outflow;
    let net_financing = financing_inflow;
    let net_change = net_operating + net_investing + net_financing;

    // 期初现金 = 所有期间前现金科目余额
    let cash_opening = accounts.get("1001").map(|(d,c)| d-c).unwrap_or(0.0)
        + accounts.get("1002").map(|(d,c)| d-c).unwrap_or(0.0)
        - net_change; // 倒推期初

    Ok(Json(json!({
        "period_from": q.from.as_deref().unwrap_or("beginning"),
        "period_to": q.to.as_deref().unwrap_or("now"),
        "opening_cash": cash_opening,
        "operating": {
            "inflow": operating_inflow,
            "outflow": operating_outflow,
            "net": net_operating,
            "items": [
                {"label": "销售收入", "amount": operating_inflow},
                {"label": "费用支出", "amount": -operating_outflow},
            ]
        },
        "investing": {
            "inflow": 0.0,
            "outflow": investing_outflow,
            "net": net_investing,
            "items": [
                {"label": "购建固定资产", "amount": -investing_outflow},
            ]
        },
        "financing": {
            "inflow": financing_inflow,
            "outflow": 0.0,
            "net": net_financing,
            "items": [
                {"label": "吸收投资", "amount": financing_inflow},
            ]
        },
        "net_change": net_change,
        "closing_cash": cash_opening + net_change,
    })))
}

/// 账龄分析 GET /api/finance/aging?as_of=2026-05-17
async fn aging_analysis(
    State(db): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<FinQuery>,
) -> Result<Json<Value>, StatusCode> {
    let p = frappe_core::storage::DocPagination { limit: 5000, offset: 0 };
    let order = frappe_core::storage::DocOrder { field: "posting_date".into(), descending: true };
    let entries = db.get_raw_list("Journal Entry", &[], &[order], &p)
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let as_of = q.as_of.as_deref().unwrap_or("");
    let as_of_date = NaiveDate::parse_from_str(as_of, "%Y-%m-%d").unwrap_or(
        chrono::Utc::now().date_naive()
    );

    // 只分析应收账款 (1122) 的贷方发生额
    let mut receivables: Vec<(String, f64, String)> = Vec::new(); // (account, amount, date)
    for entry in &entries {
        let posting = entry.get("posting_date").and_then(|v| v.as_str()).unwrap_or("");
        if posting.is_empty() || posting > as_of { continue; }

        for line in parse_journal_lines(entry) {
            if line.account == "1122" && line.debit > 0.0 {
                receivables.push((line.account.clone(), line.debit, posting.to_string()));
            }
        }
    }

    let mut aging = json!({
        "0_30": {"count": 0, "amount": 0.0},
        "31_60": {"count": 0, "amount": 0.0},
        "61_90": {"count": 0, "amount": 0.0},
        "over_90": {"count": 0, "amount": 0.0},
    });

    for (_, amount, date_str) in &receivables {
        if let Ok(d) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
            let days = (as_of_date - d).num_days();
            let bucket = if days <= 30 { "0_30" } else if days <= 60 { "31_60" } else if days <= 90 { "61_90" } else { "over_90" };
            aging[bucket]["count"] = json!(aging[bucket]["count"].as_i64().unwrap_or(0) + 1);
            aging[bucket]["amount"] = json!(aging[bucket]["amount"].as_f64().unwrap_or(0.0) + amount);
        }
    }

    let total: f64 = ["0_30","31_60","61_90","over_90"].iter()
        .map(|k| aging[k]["amount"].as_f64().unwrap_or(0.0))
        .sum();

    Ok(Json(json!({
        "as_of": as_of,
        "aging": aging,
        "total": total,
        "total_items": receivables.len(),
    })))
}
