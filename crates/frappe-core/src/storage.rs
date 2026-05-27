//! DocumentStorage — 异步文档持久化 trait
//!
//! 支持任意后端：SQLite (via spawn_blocking), PostgreSQL (native async), S3 等。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::Doctype;

/// 文档过滤器
#[derive(Debug, Clone)]
pub struct DocFilter {
    pub field: String,
    pub operator: String,
    pub value: String,
}

impl DocFilter {
    pub fn eq(field: impl Into<String>, value: impl Into<String>) -> Self {
        Self { field: field.into(), operator: "=".into(), value: value.into() }
    }
}

/// 排序
#[derive(Debug, Clone)]
pub struct DocOrder {
    pub field: String,
    pub descending: bool,
}

/// 分页
#[derive(Debug, Clone)]
pub struct DocPagination {
    pub limit: usize,
    pub offset: usize,
}

impl Default for DocPagination {
    fn default() -> Self { Self { limit: 20, offset: 0 } }
}

/// 异步文档存储 trait
#[async_trait]
pub trait DocumentStorage: Send + Sync + 'static {
    type Error: std::fmt::Display + Send;

    async fn get_doc<T: Doctype + Send>(&self, name: &str) -> Result<Option<T>, Self::Error>;
    async fn exists<T: Doctype + Send>(&self, name: &str) -> Result<bool, Self::Error>;
    async fn insert<T: Doctype + Send>(&self, doc: &T) -> Result<(), Self::Error>;
    async fn save<T: Doctype + Send>(&self, doc: &T) -> Result<(), Self::Error>;
    async fn delete<T: Doctype + Send>(&self, name: &str) -> Result<(), Self::Error>;
    async fn get_list<T: Doctype + Send>(
        &self, filters: &[DocFilter], orders: &[DocOrder], pagination: &DocPagination,
    ) -> Result<Vec<T>, Self::Error>;
    async fn count<T: Doctype + Send>(&self, filters: &[DocFilter]) -> Result<usize, Self::Error>;
    async fn get_next_series(&self, series_key: &str, padding: usize) -> Result<String, Self::Error>;
}
