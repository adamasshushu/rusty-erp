//! MySQL 存储 (sqlx) — 异步 JSON 文档存储
//! API 与 SQLite / PostgreSQL 后端一致，可无缝切换

use chrono::Utc;
use serde_json::Value;
use sqlx::mysql::{MySqlPool, MySqlPoolOptions};
use sqlx::Row;

#[derive(Clone)]
pub struct MysqlStorage {
    pool: MySqlPool,
}

impl MysqlStorage {
    pub async fn new(url: &str) -> Result<Self, sqlx::Error> {
        let pool = MySqlPoolOptions::new()
            .max_connections(10)
            .connect(url)
            .await?;

        // 建表
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS _documents (
                name VARCHAR(255) PRIMARY KEY,
                doctype VARCHAR(140) NOT NULL,
                docstatus INT DEFAULT 0,
                data JSON NOT NULL,
                owner VARCHAR(140) DEFAULT 'Administrator',
                creation DATETIME DEFAULT CURRENT_TIMESTAMP,
                modified DATETIME DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4"
        ).execute(&pool).await?;

        let _ = sqlx::query(
            "CREATE INDEX idx_doctype ON _documents(doctype)"
        ).execute(&pool).await;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS _series (
                key_name VARCHAR(255) PRIMARY KEY,
                current_val BIGINT NOT NULL DEFAULT 0
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4"
        ).execute(&pool).await?;

        // 审批工作流
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS _approvals (
                id INT AUTO_INCREMENT PRIMARY KEY,
                doctype VARCHAR(140) NOT NULL,
                doc_name VARCHAR(255) NOT NULL,
                approver VARCHAR(140),
                approval_level INT DEFAULT 1,
                status VARCHAR(20) DEFAULT 'pending',
                comment TEXT,
                creation DATETIME DEFAULT CURRENT_TIMESTAMP,
                modified DATETIME DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
                UNIQUE KEY unique_level (doctype, doc_name, approval_level)
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4"
        ).execute(&pool).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS _workflow_config (
                doctype VARCHAR(140) PRIMARY KEY,
                levels INT DEFAULT 2,
                approvers JSON
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4"
        ).execute(&pool).await?;

        // 默认工作流配置：销售发票/采购发票 2 级审批
        let _ = sqlx::query(
            "INSERT IGNORE INTO _workflow_config (doctype, levels, approvers) VALUES (?, 2, '[\"销售经理\",\"财务主管\"]')"
        ).bind("Sales Invoice").execute(&pool).await;
        let _ = sqlx::query(
            "INSERT IGNORE INTO _workflow_config (doctype, levels, approvers) VALUES (?, 2, '[\"采购经理\",\"财务主管\"]')"
        ).bind("Purchase Invoice").execute(&pool).await;
        let _ = sqlx::query(
            "INSERT IGNORE INTO _workflow_config (doctype, levels, approvers) VALUES (?, 2, '[\"部门主管\",\"财务主管\"]')"
        ).bind("Asset").execute(&pool).await;

        Ok(Self { pool })
    }

    pub fn pool(&self) -> &MySqlPool { &self.pool }

    // ── CRUD ──

    pub async fn get_raw_doc(&self, doctype: &str, name: &str) -> Result<Option<Value>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT data FROM _documents WHERE name = ? AND doctype = ?"
        ).bind(name).bind(doctype)
         .fetch_optional(&self.pool).await?;
        Ok(row.and_then(|r| {
            let raw: serde_json::Value = r.get("data");
            Some(raw)
        }))
    }

    pub async fn insert_raw(&self, name: &str, doctype: &str, data: &Value) -> Result<(), sqlx::Error> {
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        sqlx::query(
            "INSERT IGNORE INTO _documents (name, doctype, data, creation, modified) VALUES (?, ?, ?, ?, ?)"
        ).bind(name).bind(doctype).bind(data).bind(&now).bind(&now)
         .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn save_raw(&self, name: &str, doctype: &str, data: &Value) -> Result<(), sqlx::Error> {
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        sqlx::query(
            "UPDATE _documents SET data = ?, modified = ? WHERE name = ? AND doctype = ?"
        ).bind(data).bind(&now).bind(name).bind(doctype)
         .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn delete_raw(&self, doctype: &str, name: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM _documents WHERE name = ? AND doctype = ?")
            .bind(name).bind(doctype).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn get_raw_list(
        &self, doctype: &str, filters: &[frappe_core::storage::DocFilter],
        orders: &[frappe_core::storage::DocOrder],
        pagination: &frappe_core::storage::DocPagination,
    ) -> Result<Vec<Value>, sqlx::Error> {
        let mut sql = String::from("SELECT data FROM _documents WHERE doctype = ?");
        let mut binds: Vec<String> = vec![];
        for f in filters {
            match f.operator.as_str() {
                "=" => {
                    sql.push_str(" AND JSON_UNQUOTE(JSON_EXTRACT(data, ?)) = ?");
                    binds.push(format!("$.{}", f.field));
                    binds.push(f.value.clone());
                }
                "like" => {
                    sql.push_str(" AND JSON_UNQUOTE(JSON_EXTRACT(data, ?)) LIKE ?");
                    binds.push(format!("$.{}", f.field));
                    binds.push(format!("%{}%", f.value));
                }
                ">" => {
                    sql.push_str(" AND CAST(JSON_UNQUOTE(JSON_EXTRACT(data, ?)) AS DECIMAL(20,2)) > ?");
                    binds.push(format!("$.{}", f.field));
                    binds.push(f.value.clone());
                }
                "<" => {
                    sql.push_str(" AND CAST(JSON_UNQUOTE(JSON_EXTRACT(data, ?)) AS DECIMAL(20,2)) < ?");
                    binds.push(format!("$.{}", f.field));
                    binds.push(f.value.clone());
                }
                _ => {}
            }
        }
        if !orders.is_empty() {
            sql.push_str(" ORDER BY ");
            for (i, o) in orders.iter().enumerate() {
                if i > 0 { sql.push_str(", "); }
                sql.push_str(&format!("JSON_UNQUOTE(JSON_EXTRACT(data, '$.{}')) {}", o.field, if o.descending { "DESC" } else { "ASC" }));
            }
        } else { sql.push_str(" ORDER BY modified DESC"); }
        sql.push_str(&format!(" LIMIT {} OFFSET {}", pagination.limit, pagination.offset));
        let mut query = sqlx::query_as::<_, (serde_json::Value,)>(&sql).bind(doctype);
        for b in &binds { query = query.bind(b); }
        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|(data,)| data).collect())
    }

    pub async fn search_raw(
        &self, doctype: &str, query_str: &str,
        orders: &[frappe_core::storage::DocOrder], pagination: &frappe_core::storage::DocPagination,
    ) -> Result<Vec<Value>, sqlx::Error> {
        let mut sql = String::from(
            "SELECT data FROM _documents WHERE doctype = ? AND (\
                name LIKE ? OR \
                JSON_UNQUOTE(JSON_EXTRACT(data, '$.customer_name')) LIKE ? OR \
                JSON_UNQUOTE(JSON_EXTRACT(data, '$.item_name')) LIKE ? OR \
                JSON_UNQUOTE(JSON_EXTRACT(data, '$.supplier_name')) LIKE ? OR \
                JSON_UNQUOTE(JSON_EXTRACT(data, '$.company')) LIKE ?\
            )"
        );
        if !orders.is_empty() {
            sql.push_str(&format!(" ORDER BY JSON_UNQUOTE(JSON_EXTRACT(data, '$.{}')) {}", orders[0].field, if orders[0].descending { "DESC" } else { "ASC" }));
        } else { sql.push_str(" ORDER BY modified DESC"); }
        sql.push_str(&format!(" LIMIT {} OFFSET {}", pagination.limit, pagination.offset));
        let like = format!("%{}%", query_str);
        let rows = sqlx::query_as::<_, (serde_json::Value,)>(&sql)
            .bind(doctype).bind(&like).bind(&like).bind(&like).bind(&like).bind(&like)
            .fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|(data,)| data).collect())
    }

    pub async fn get_raw_count(&self, doctype: &str) -> Result<usize, sqlx::Error> {
        let row = sqlx::query("SELECT COUNT(*) as cnt FROM _documents WHERE doctype = ?")
            .bind(doctype).fetch_one(&self.pool).await?;
        Ok(row.get::<i64, _>("cnt") as usize)
    }

    pub async fn get_raw_count_filtered(&self, doctype: &str, query_str: &str) -> Result<usize, sqlx::Error> {
        let like = format!("%{}%", query_str);
        let row = sqlx::query(
            "SELECT COUNT(*) as cnt FROM _documents WHERE doctype = ? AND (\
                name LIKE ? OR \
                JSON_UNQUOTE(JSON_EXTRACT(data, '$.customer_name')) LIKE ? OR \
                JSON_UNQUOTE(JSON_EXTRACT(data, '$.item_name')) LIKE ? OR \
                JSON_UNQUOTE(JSON_EXTRACT(data, '$.supplier_name')) LIKE ? OR \
                JSON_UNQUOTE(JSON_EXTRACT(data, '$.company')) LIKE ?\
            )"
        ).bind(doctype).bind(&like).bind(&like).bind(&like).bind(&like).bind(&like)
         .fetch_one(&self.pool).await?;
        Ok(row.get::<i64, _>("cnt") as usize)
    }

    // ── 命名序列：如 ACC-SINV-.YYYY.- → ACC-SINV-2026-00001 ──
    pub async fn get_next_series(&self, series_pattern: &str) -> Result<String, sqlx::Error> {
        use chrono::Local;
        let now = Local::now();
        let key = series_pattern
            .replace(".YYYY.", &now.format("%Y").to_string())
            .replace(".YY.", &now.format("%y").to_string())
            .replace(".MM.", &now.format("%m").to_string())
            .replace(".DD.", &now.format("%d").to_string());

        sqlx::query(
            "INSERT INTO _series (key_name, current_val) VALUES (?, 1) ON DUPLICATE KEY UPDATE current_val = current_val + 1"
        ).bind(&key).execute(&self.pool).await?;

        let row = sqlx::query("SELECT current_val FROM _series WHERE key_name = ?")
            .bind(&key).fetch_one(&self.pool).await?;
        let seq: i64 = row.get("current_val");
        Ok(format!("{}{:05}", key, seq))
    }

    // ── 审批工作流 ──

    /// 获取工作流配置（审批级数和审批人）
    pub async fn get_workflow_config(&self, doctype: &str) -> Result<(i32, Vec<String>), sqlx::Error> {
        let row = sqlx::query(
            "SELECT levels, approvers FROM _workflow_config WHERE doctype = ?"
        ).bind(doctype).fetch_optional(&self.pool).await?;
        if let Some(r) = row {
            let levels: i32 = r.get("levels");
            let approvers_raw: Option<serde_json::Value> = r.get("approvers");
            let approvers: Vec<String> = approvers_raw
                .and_then(|v| v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect()))
                .unwrap_or_default();
            Ok((levels, approvers))
        } else {
            Ok((0, vec![]))
        }
    }

    /// 为文档创建待审批记录（多级）
    pub async fn create_approvals(&self, doctype: &str, doc_name: &str, levels: i32) -> Result<(), sqlx::Error> {
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        for level in 1..=levels {
            sqlx::query(
                "INSERT INTO _approvals (doctype, doc_name, approval_level, status, creation, modified) \
                 VALUES (?, ?, ?, 'pending', ?, ?) \
                 ON DUPLICATE KEY UPDATE status='pending', approver=NULL, comment=NULL, modified=?"
            ).bind(doctype).bind(doc_name).bind(level).bind(&now).bind(&now).bind(&now)
             .execute(&self.pool).await?;
        }
        Ok(())
    }

    /// 获取文档的所有审批记录
    pub async fn get_approvals(&self, doctype: &str, doc_name: &str) -> Result<Vec<serde_json::Value>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT approval_level, approver, status, comment, \
             DATE_FORMAT(creation, '%Y-%m-%d %H:%i:%s') as creation \
             FROM _approvals WHERE doctype = ? AND doc_name = ? ORDER BY approval_level"
        ).bind(doctype).bind(doc_name).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(|r| serde_json::json!({
            "level": r.get::<i32, _>("approval_level"),
            "approver": r.get::<Option<String>, _>("approver"),
            "status": r.get::<String, _>("status"),
            "comment": r.get::<Option<String>, _>("comment"),
            "creation": r.get::<Option<String>, _>("creation").unwrap_or_default(),
        })).collect())
    }

    /// 审批某一级（当前用户审批）
    pub async fn approve_level(&self, doctype: &str, doc_name: &str, level: i32, user: &str, comment: &str) -> Result<bool, sqlx::Error> {
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let result = sqlx::query(
            "UPDATE _approvals SET status='approved', approver=?, comment=?, modified=? \
             WHERE doctype=? AND doc_name=? AND approval_level=? AND status='pending'"
        ).bind(user).bind(comment).bind(&now).bind(doctype).bind(doc_name).bind(level)
         .execute(&self.pool).await?;
        Ok(result.rows_affected() > 0)
    }

    /// 驳回：将所有待审批记录标记为 rejected，退回草稿
    pub async fn reject_all(&self, doctype: &str, doc_name: &str, user: &str, comment: &str) -> Result<(), sqlx::Error> {
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        sqlx::query(
            "UPDATE _approvals SET status='rejected', approver=?, comment=?, modified=? \
             WHERE doctype=? AND doc_name=? AND status='pending'"
        ).bind(user).bind(comment).bind(&now).bind(doctype).bind(doc_name)
         .execute(&self.pool).await?;
        Ok(())
    }

    /// 检查是否所有级别都已审批通过
    pub async fn is_fully_approved(&self, doctype: &str, doc_name: &str) -> Result<bool, sqlx::Error> {
        let row = sqlx::query(
            "SELECT COUNT(*) as cnt FROM _approvals \
             WHERE doctype=? AND doc_name=? AND status!='approved'"
        ).bind(doctype).bind(doc_name).fetch_one(&self.pool).await?;
        let pending: i64 = row.get("cnt");
        Ok(pending == 0)
    }
}
