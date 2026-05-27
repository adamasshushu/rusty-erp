//! Frappe 核心 — Doctype 抽象层
//!
//! 对标 Frappe Python 的 `document.py` + `base_document.py`.
//! 定义了所有 ERP 文档共有的生命周期：
//!   New → Draft → Submitted → Cancelled

use serde::{Deserialize, Serialize};

/// 文档状态 — 对标 Python 的 `DocStatus`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocStatus {
    Draft = 0,
    Submitted = 1,
    Cancelled = 2,
}

impl DocStatus {
    pub fn as_int(&self) -> i32 {
        *self as i32
    }
    pub fn from_int(v: i32) -> Self {
        match v {
            0 => DocStatus::Draft,
            1 => DocStatus::Submitted,
            2 => DocStatus::Cancelled,
            _ => DocStatus::Draft,
        }
    }
}

/// 权限级别 — 对标 Python `permissions.py` 的 15 级权限
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Select,
    Read,
    Write,
    Create,
    Delete,
    Submit,
    Cancel,
    Amend,
    Print,
    Email,
    Report,
    Import,
    Export,
    Share,
}

/// Doctype 元数据 — 从 JSON 编译时提取的关键信息
#[derive(Debug, Clone)]
pub struct DoctypeMeta {
    /// Doctype 名称, e.g. "Sales Invoice"
    pub name: &'static str,
    /// 数据库表名, e.g. "tabSales Invoice"
    pub table_name: &'static str,
    /// 是否单例 (Single doctype)
    pub is_single: bool,
    /// 是否有子表 (child tables)
    pub has_children: bool,
    /// 命名规则: naming_series / autoname
    pub autoname: Option<&'static str>,
}

/// Doctype trait — 所有 ERP 文档必须实现
///
/// # Safety
/// 由 `#[derive(Doctype)]` 自动实现，不手动编写。
///
/// # Example
/// ```ignore
/// #[derive(Doctype, Serialize, Deserialize)]
/// #[doctype(name = "Sales Invoice", table = "tabSales Invoice")]
/// pub struct SalesInvoice { ... }
/// ```
pub trait Doctype: Serialize + for<'de> Deserialize<'de> + Send + Sync {
    /// 返回元数据
    fn meta() -> &'static DoctypeMeta;

    /// 文档唯一名称 (name field)
    fn name(&self) -> &str;

    /// 当前状态
    fn docstatus(&self) -> DocStatus;

    /// 验证文档 (字段约束 + 业务规则)
    fn validate(&self) -> Result<(), Vec<String>> {
        Ok(())
    }

    /// 提交前检查
    fn before_submit(&self) -> Result<(), Vec<String>> {
        Ok(())
    }

    /// 提交
    fn on_submit(&mut self);

    /// 取消前检查
    fn before_cancel(&self) -> Result<(), Vec<String>> {
        Ok(())
    }

    /// 取消
    fn on_cancel(&mut self);

    /// 保存前检查
    fn before_save(&self) -> Result<(), Vec<String>> {
        Ok(())
    }

    /// 插入前检查
    fn before_insert(&self) -> Result<(), Vec<String>> {
        Ok(())
    }

    /// 检查当前用户是否有指定权限
    fn check_permission(&self, _perm: Permission) -> bool {
        true
    }
}

/// 子表 trait — 对标 Python 的 child table fields
pub trait ChildTable: Serialize + for<'de> Deserialize<'de> + Clone + Send + Sync {
    fn parent_field() -> &'static str;
}

pub mod storage;

// Re-export the derive macro
pub use frappe_derive::Doctype;
