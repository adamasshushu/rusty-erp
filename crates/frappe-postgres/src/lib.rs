//! PostgreSQL 存储后端 — JSONB 文档存储 + 命名序列
//!
//! 使用 sqlx 异步连接池，表结构:
//!   _documents (name TEXT PRIMARY KEY, doctype TEXT, docstatus INT,
//!               data JSONB, owner TEXT, creation TIMESTAMPTZ, modified TIMESTAMPTZ)
//!   _series (key TEXT PRIMARY KEY, current INT)

use async_trait::async_trait;
use chrono::Utc;
use frappe_core::storage::{DocFilter, DocOrder, DocPagination, DocumentStorage};
use serde_json::Value;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;

pub struct PostgresStorage {
    pool: PgPool,
}

impl PostgresStorage {
    /// 创建连接池并初始化表结构
    pub async fn new(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await?;

        // 建表
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS _documents (
                name TEXT PRIMARY KEY,
                doctype TEXT NOT NULL,
                docstatus INT DEFAULT 0,
                data JSONB NOT NULL DEFAULT '{}',
                owner TEXT DEFAULT 'Administrator',
                creation TIMESTAMPTZ DEFAULT NOW(),
                modified TIMESTAMPTZ DEFAULT NOW()
            )"
        ).execute(&pool).await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_doctype ON _documents(doctype)"
        ).execute(&pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS _series (
                key TEXT PRIMARY KEY,
                current INT NOT NULL DEFAULT 0
            )"
        ).execute(&pool).await?;

        Ok(Self { pool })
    }

    pub fn pool(&self) -> &PgPool { &self.pool }

    /// Raw CRUD — 供 app 层直接使用
    pub async fn get_raw_doc(&self, doctype: &str, name: &str) -> Result<Option<Value>, sqlx::Error> {
        let row = sqlx::query("SELECT data FROM _documents WHERE name = $1 AND doctype = $2")
            .bind(name).bind(doctype)
            .fetch_optional(&self.pool).await?;
        Ok(row.map(|r| r.get::<Value, _>("data")))
    }

    pub async fn insert_raw(&self, name: &str, doctype: &str, data: &Value) -> Result<(), sqlx::Error> {
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO _documents (name, doctype, data, creation, modified)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (name) DO NOTHING"
        ).bind(name).bind(doctype).bind(data).bind(now).bind(now)
         .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn save_raw(&self, name: &str, doctype: &str, data: &Value) -> Result<(), sqlx::Error> {
        let now = Utc::now();
        sqlx::query(
            "UPDATE _documents SET data = $1, modified = $2 WHERE name = $3 AND doctype = $4"
        ).bind(data).bind(now).bind(name).bind(doctype)
         .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn delete_raw(&self, doctype: &str, name: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM _documents WHERE name = $1 AND doctype = $2")
            .bind(name).bind(doctype)
            .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn get_raw_list(
        &self, doctype: &str, filters: &[DocFilter],
        orders: &[DocOrder], pagination: &DocPagination,
    ) -> Result<Vec<Value>, sqlx::Error> {
        let mut sql = String::from("SELECT data FROM _documents WHERE doctype = $1");
        let mut params: Vec<String> = vec![doctype.to_string()];
        let mut param_idx = 2;

        for f in filters {
            match f.operator.as_str() {
                "=" => {
                    sql.push_str(&format!(" AND data->>'{}' = ${}", f.field, param_idx));
                    params.push(f.value.clone());
                    param_idx += 1;
                }
                "like" => {
                    sql.push_str(&format!(" AND data->>'{}' LIKE ${}", f.field, param_idx));
                    params.push(format!("%{}%", f.value));
                    param_idx += 1;
                }
                ">" => {
                    sql.push_str(&format!(" AND (data->>'{}')::numeric > ${}", f.field, param_idx));
                    params.push(f.value.clone());
                    param_idx += 1;
                }
                "<" => {
                    sql.push_str(&format!(" AND (data->>'{}')::numeric < ${}", f.field, param_idx));
                    params.push(f.value.clone());
                    param_idx += 1;
                }
                _ => {}
            }
        }

        if !orders.is_empty() {
            sql.push_str(" ORDER BY ");
            for (i, o) in orders.iter().enumerate() {
                if i > 0 { sql.push_str(", "); }
                sql.push_str(&format!("data->>'{}' {}", o.field, if o.descending { "DESC" } else { "ASC" }));
            }
        } else {
            sql.push_str(" ORDER BY modified DESC");
        }
        sql.push_str(&format!(" LIMIT {} OFFSET {}", pagination.limit, pagination.offset));

        let mut query = sqlx::query(&sql);
        for p in &params {
            query = query.bind(p);
        }
        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|r| r.get::<Value, _>("data")).collect())
    }

    pub async fn search_raw(
        &self, doctype: &str, query_str: &str,
        orders: &[DocOrder], pagination: &DocPagination,
    ) -> Result<Vec<Value>, sqlx::Error> {
        let sql = format!(
            "SELECT data FROM _documents WHERE doctype = $1 AND (
                name ILIKE $2 OR
                data->>'customer_name' ILIKE $2 OR
                data->>'item_name' ILIKE $2 OR
                data->>'supplier_name' ILIKE $2 OR
                data->>'company' ILIKE $2
            ) ORDER BY {} {} LIMIT {} OFFSET {}",
            if orders.is_empty() { "modified".into() } else { orders[0].field.clone() },
            if orders.is_empty() || !orders[0].descending { "DESC" } else { "ASC" },
            pagination.limit, pagination.offset,
        );
        let rows = sqlx::query(&sql)
            .bind(doctype)
            .bind(format!("%{}%", query_str))
            .fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|r| r.get::<Value, _>("data")).collect())
    }

    pub async fn get_raw_count(&self, doctype: &str) -> Result<usize, sqlx::Error> {
        let row = sqlx::query("SELECT COUNT(*) as cnt FROM _documents WHERE doctype = $1")
            .bind(doctype).fetch_one(&self.pool).await?;
        Ok(row.get::<i64, _>("cnt") as usize)
    }

    pub async fn get_raw_count_filtered(&self, doctype: &str, query_str: &str) -> Result<usize, sqlx::Error> {
        let row = sqlx::query(
            "SELECT COUNT(*) as cnt FROM _documents WHERE doctype = $1 AND (
                name ILIKE $2 OR data->>'customer_name' ILIKE $2 OR
                data->>'item_name' ILIKE $2 OR data->>'supplier_name' ILIKE $2 OR
                data->>'company' ILIKE $2)"
        ).bind(doctype).bind(format!("%{}%", query_str))
         .fetch_one(&self.pool).await?;
        Ok(row.get::<i64, _>("cnt") as usize)
    }
}

#[async_trait]
impl DocumentStorage for PostgresStorage {
    type Error = sqlx::Error;

    async fn get_doc<T: frappe_core::Doctype + Send>(&self, name: &str) -> Result<Option<T>, Self::Error> {
        let row = sqlx::query("SELECT data FROM _documents WHERE name = $1")
            .bind(name).fetch_optional(&self.pool).await?;
        match row {
            Some(r) => {
                let data: Value = r.get("data");
                Ok(serde_json::from_value(data).ok())
            }
            None => Ok(None),
        }
    }

    async fn exists<T: frappe_core::Doctype + Send>(&self, name: &str) -> Result<bool, Self::Error> {
        let row = sqlx::query("SELECT 1 FROM _documents WHERE name = $1")
            .bind(name).fetch_optional(&self.pool).await?;
        Ok(row.is_some())
    }

    async fn insert<T: frappe_core::Doctype + Send>(&self, doc: &T) -> Result<(), Self::Error> {
        let data = serde_json::to_value(doc).unwrap_or_default();
        self.insert_raw(&doc.name(), &T::meta().name, &data).await
    }

    async fn save<T: frappe_core::Doctype + Send>(&self, doc: &T) -> Result<(), Self::Error> {
        let data = serde_json::to_value(doc).unwrap_or_default();
        self.save_raw(&doc.name(), &T::meta().name, &data).await
    }

    async fn delete<T: frappe_core::Doctype + Send>(&self, name: &str) -> Result<(), Self::Error> {
        self.delete_raw(&T::meta().name, name).await
    }

    async fn get_list<T: frappe_core::Doctype + Send>(
        &self, filters: &[DocFilter], orders: &[DocOrder], pagination: &DocPagination,
    ) -> Result<Vec<T>, Self::Error> {
        let values = self.get_raw_list(&T::meta().name, filters, orders, pagination).await?;
        Ok(values.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
    }

    async fn count<T: frappe_core::Doctype + Send>(&self, _filters: &[DocFilter]) -> Result<usize, Self::Error> {
        let row = sqlx::query("SELECT COUNT(*) as cnt FROM _documents WHERE doctype = $1")
            .bind(T::meta().name).fetch_one(&self.pool).await?;
        Ok(row.get::<i64, _>("cnt") as usize)
    }

    async fn get_next_series(&self, series_key: &str, padding: usize) -> Result<String, Self::Error> {
        let row = sqlx::query(
            "INSERT INTO _series (key, current) VALUES ($1, 1)
             ON CONFLICT (key) DO UPDATE SET current = _series.current + 1
             RETURNING current"
        ).bind(series_key).fetch_one(&self.pool).await?;
        let current: i32 = row.get("current");
        Ok(format!("{:0width$}", current, width = padding))
    }
}
