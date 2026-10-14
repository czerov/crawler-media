//! 发布前的 scope 捕获与持久化签名复核（P2-4）。
//!
//! Movie/TV 与 Video 共用同一条绑定路径：任何文件被登记之前都必须完成复核。

use axum::http::StatusCode;

use crate::http::err;
use crate::management::ApiState;

use super::outcome::{ScanRunError, failed_response, presence_error};
use super::super::scope::ScanScope;

/// 捕获 scope 并把它绑定到持久化 signature（P2-4）。
pub(super) fn capture_and_verify_scope(
    state: &ApiState,
    id: &str,
    op_id: Option<&str>,
) -> Result<ScanScope, ScanRunError> {
    let scan_scope = ScanScope::capture(state, id).map_err(|error| {
        let summary = format!("{error:?}");
        failed_response(presence_error(error), summary)
    })?;
    verify_captured_scope(state, op_id, &scan_scope)?;
    Ok(scan_scope)
}

/// 发布前复核：持久化 signature 必须等于刚捕获的 scope（P2-4）。
///
/// 准入（`scan_admission`）释放 Store 锁之后、`ScanScope::capture` 之前，配置可能被
/// PATCH；此时旧 operation 绝不能拿新 roots 登记文件。
///
/// 读不到 operation 或 operation 已消失时必须**失败关闭**（P2-4）：`ScanScope::validate`
/// 只能证明捕获 scope 与当前配置一致，不能替代「捕获 scope == 持久化 signature」，
/// 放行等于让一个无法对账的 operation 继续登记文件。
fn verify_captured_scope(
    state: &ApiState,
    op_id: Option<&str>,
    scan_scope: &ScanScope,
) -> Result<(), ScanRunError> {
    let Some(op_id) = op_id else { return Ok(()) };
    let persisted = {
        let store = state.store.lock();
        store.scan_operation(op_id)
    };
    match persisted {
        Ok(Some(op)) if op.scope_signature != scan_scope.signature() => {
            let reason = format!(
                "captured scope changed: persisted={} captured={}",
                op.scope_signature,
                scan_scope.signature()
            );
            tracing::warn!(%op_id, %reason, "捕获 scope 与持久化 signature 不一致，取消过期 operation");
            Err(ScanRunError::ScopeChanged { reason })
        }
        Ok(Some(_)) => Ok(()),
        Ok(None) => {
            tracing::error!(%op_id, "扫描 operation 已不存在，拒绝在无法对账的情况下发布");
            Err(failed_response(
                err(
                    StatusCode::CONFLICT,
                    "library.scan_operation_missing",
                    "扫描 operation 不存在",
                ),
                "library.scan_operation_missing",
            ))
        }
        Err(error) => {
            tracing::error!(%error, %op_id, "读取扫描 operation 失败，拒绝在无法对账的情况下发布");
            Err(failed_response(
                err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error.to_string()),
                format!("scan operation unreadable: {error}"),
            ))
        }
    }
}
