//! SQLite 存储 (sqlx) — 异步 JSON 文档存储
//! 表结构与 PostgreSQL 后端一致，可无缝切换

use chrono::Utc;
use serde_json::Value;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use sqlx::Row;

#[derive(Clone)]
pub struct SqliteStorage {
    pool: SqlitePool,
}

impl SqliteStorage {
    pub async fn new(path: &str) -> Result<Self, sqlx::Error> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&format!("sqlite:{}?mode=rwc", path))
            .await?;

        // WAL mode for better concurrent reads
        sqlx::query("PRAGMA journal_mode=WAL").execute(&pool).await?;
        sqlx::query("PRAGMA busy_timeout=5000").execute(&pool).await?;

        // 建表
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS _documents (
                name TEXT PRIMARY KEY,
                doctype TEXT NOT NULL,
                docstatus INTEGER DEFAULT 0,
                data TEXT NOT NULL DEFAULT '{}',
                owner TEXT DEFAULT 'Administrator',
                creation TEXT DEFAULT (datetime('now')),
                modified TEXT DEFAULT (datetime('now'))
            )"
        ).execute(&pool).await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_doctype ON _documents(doctype)"
        ).execute(&pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS _series (
                key TEXT PRIMARY KEY,
                current INTEGER NOT NULL DEFAULT 0
            )"
        ).execute(&pool).await?;

        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool { &self.pool }

    // ── Raw CRUD (JSON, no type binding) ──

    pub async fn get_raw_doc(&self, doctype: &str, name: &str) -> Result<Option<Value>, sqlx::Error> {
        let row = sqlx::query("SELECT data FROM _documents WHERE name = ?1 AND doctype = ?2")
            .bind(name).bind(doctype)
            .fetch_optional(&self.pool).await?;
        Ok(row.and_then(|r| {
            let s: String = r.get("data");
            serde_json::from_str(&s).ok()
        }))
    }

    pub async fn insert_raw(&self, name: &str, doctype: &str, data: &Value) -> Result<(), sqlx::Error> {
        let now = Utc::now().to_rfc3339();
        let json_str = serde_json::to_string(data).unwrap_or_default();
        sqlx::query(
            "INSERT OR IGNORE INTO _documents (name, doctype, data, creation, modified) VALUES (?1, ?2, ?3, ?4, ?5)"
        ).bind(name).bind(doctype).bind(&json_str).bind(&now).bind(&now)
         .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn save_raw(&self, name: &str, doctype: &str, data: &Value) -> Result<(), sqlx::Error> {
        let now = Utc::now().to_rfc3339();
        let json_str = serde_json::to_string(data).unwrap_or_default();
        sqlx::query(
            "UPDATE _documents SET data = ?1, modified = ?2 WHERE name = ?3 AND doctype = ?4"
        ).bind(&json_str).bind(&now).bind(name).bind(doctype)
         .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn delete_raw(&self, doctype: &str, name: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM _documents WHERE name = ?1 AND doctype = ?2")
            .bind(name).bind(doctype).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn get_raw_list(
        &self, doctype: &str, filters: &[frappe_core::storage::DocFilter], orders: &[frappe_core::storage::DocOrder],
        pagination: &frappe_core::storage::DocPagination,
    ) -> Result<Vec<Value>, sqlx::Error> {
        let mut sql = String::from("SELECT data FROM _documents WHERE doctype = ?1");
        let mut param_idx = 2;

        for f in filters {
            match f.operator.as_str() {
                "=" => {
                    sql.push_str(&format!(" AND json_extract(data, '$.{}') = ?{}", f.field, param_idx));
                    param_idx += 1;
                }
                "like" => {
                    sql.push_str(&format!(" AND json_extract(data, '$.{}') LIKE ?{}", f.field, param_idx));
                    param_idx += 1;
                }
                ">" => {
                    sql.push_str(&format!(" AND CAST(json_extract(data, '$.{}') AS REAL) > ?{}", f.field, param_idx));
                    param_idx += 1;
                }
                "<" => {
                    sql.push_str(&format!(" AND CAST(json_extract(data, '$.{}') AS REAL) < ?{}", f.field, param_idx));
                    param_idx += 1;
                }
                _ => {}
            }
        }

        if !orders.is_empty() {
            sql.push_str(" ORDER BY ");
            for (i, o) in orders.iter().enumerate() {
                if i > 0 { sql.push_str(", "); }
                sql.push_str(&format!("json_extract(data, '$.{}') {}", o.field, if o.descending { "DESC" } else { "ASC" }));
            }
        } else {
            sql.push_str(" ORDER BY modified DESC");
        }
        sql.push_str(&format!(" LIMIT {} OFFSET {}", pagination.limit, pagination.offset));

        // Build query with dynamic params
        let mut query = sqlx::query(&sql).bind(doctype);
        for f in filters {
            let val = if f.operator == "like" { format!("%{}%", f.value) } else { f.value.clone() };
            query = query.bind(val);
        }
        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows.into_iter().filter_map(|r| {
            let s: String = r.get("data");
            serde_json::from_str(&s).ok()
        }).collect())
    }

    pub async fn search_raw(
        &self, doctype: &str, query_str: &str,
        orders: &[frappe_core::storage::DocOrder], pagination: &frappe_core::storage::DocPagination,
    ) -> Result<Vec<Value>, sqlx::Error> {
        let mut sql = String::from(
            "SELECT data FROM _documents WHERE doctype = ?1 AND (name LIKE ?2 OR json_extract(data, '$.customer_name') LIKE ?2 OR json_extract(data, '$.item_name') LIKE ?2 OR json_extract(data, '$.supplier_name') LIKE ?2 OR json_extract(data, '$.company') LIKE ?2)"
        );
        if !orders.is_empty() {
            sql.push_str(&format!(" ORDER BY json_extract(data, '$.{}') {}", orders[0].field, if orders[0].descending { "DESC" } else { "ASC" }));
        } else {
            sql.push_str(" ORDER BY modified DESC");
        }
        sql.push_str(&format!(" LIMIT {} OFFSET {}", pagination.limit, pagination.offset));

        let like = format!("%{}%", query_str);
        let rows = sqlx::query(&sql).bind(doctype).bind(&like).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().filter_map(|r| {
            let s: String = r.get("data");
            serde_json::from_str(&s).ok()
        }).collect())
    }

    pub async fn get_raw_count(&self, doctype: &str) -> Result<usize, sqlx::Error> {
        let row = sqlx::query("SELECT COUNT(*) as cnt FROM _documents WHERE doctype = ?1")
            .bind(doctype).fetch_one(&self.pool).await?;
        Ok(row.get::<i64, _>("cnt") as usize)
    }

    pub async fn get_raw_count_filtered(&self, doctype: &str, query_str: &str) -> Result<usize, sqlx::Error> {
        let like = format!("%{}%", query_str);
        let row = sqlx::query(
            "SELECT COUNT(*) as cnt FROM _documents WHERE doctype = ?1 AND (name LIKE ?2 OR json_extract(data, '$.customer_name') LIKE ?2 OR json_extract(data, '$.item_name') LIKE ?2 OR json_extract(data, '$.supplier_name') LIKE ?2 OR json_extract(data, '$.company') LIKE ?2)"
        ).bind(doctype).bind(&like).fetch_one(&self.pool).await?;
        Ok(row.get::<i64, _>("cnt") as usize)
    }
}
