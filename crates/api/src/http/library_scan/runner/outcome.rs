//! 一次扫描运行的终态载体与失败响应映射。
//!
//! `ScanRunError` 是执行器交回 coordinator 的唯一失败合同：`ScopeChanged` 必须取消且
//! 零登记，`Failed` 携带已提交前缀的报告（P3-3 / P2-4）。

use axum::http::StatusCode;
use axum::response::Response;
use serde_json::{Value, json};

use crate::http::err;

use super::walk::ChunkTotals;

/// 扫描执行失败：保留已提交前缀的报告与错误摘要（P3-3）。
pub(crate) struct ScanRunFailure {
    pub response: Response,
    pub report: Value,
    pub error: String,
}

/// 扫描执行错误。
pub(crate) enum ScanRunError {
    /// 持久化 signature 与捕获 scope 不一致（P2-4）：必须取消且零登记。
    ScopeChanged { reason: String },
    Failed(ScanRunFailure),
}

/// 失败但无已提交前缀（入口级错误）。
pub(super) fn failed_response(response: Response, error: impl Into<String>) -> ScanRunError {
    ScanRunError::Failed(ScanRunFailure {
        response,
        report: json!({}),
        error: error.into(),
    })
}

/// Library 归属/存在性错误 → HTTP 语义（捕获 scope 失败也走这里）。
pub(super) fn presence_error(error: crate::library_presence::PresenceError) -> Response {
    use crate::library_presence::PresenceError;
    match error {
        PresenceError::Deleting => err(StatusCode::CONFLICT, "library.deleting", "Library 正在删除"),
        PresenceError::ScopeChanged => {
            err(StatusCode::CONFLICT, "library.scope_changed", "Library 配置变化")
        }
        PresenceError::Ambiguous => err(
            StatusCode::CONFLICT,
            "library.ownership_ambiguous",
            "媒体库归属歧义",
        ),
        other => err(StatusCode::CONFLICT, "library.scan", &format!("{other:?}")),
    }
}

/// 本轮扫描已提交前缀的累计状态。
#[derive(Default)]
pub(super) struct ScanRun {
    pub(super) root_reports: Vec<library::ScanRootReport>,
    pub(super) totals: ChunkTotals,
    pub(super) any_readable: bool,
}

impl ScanRun {
    /// 用当前已提交前缀构造报告（失败终态也要保留它，P3-3）。
    pub(super) fn partial_report(&self) -> library::ScanReport {
        let total_errors: usize = self.root_reports.iter().map(|r| r.error_count).sum();
        library::ScanReport {
            roots: self.root_reports.clone(),
            scanned: self.totals.scanned,
            inserted: self.totals.inserted,
            restored: 0,
            probe_pending: self.totals.inserted,
            error_count: total_errors,
            errors_truncated: false,
        }
    }
}
