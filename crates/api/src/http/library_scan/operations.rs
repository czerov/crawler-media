//! HTTP handlers for scan operation polling and cancellation.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::http::{err, ok};
use crate::management::ApiState;

pub(crate) async fn get_scan_operation(
    State(state): State<ApiState>,
    Path((_lib_id, op_id)): Path<(String, String)>,
) -> Response {
    let store = state.store.lock();
    match store.scan_operation(&op_id) {
        Ok(Some(op)) => ok(json!({
            "operation_id": op.id,
            "status": op.status.as_str(),
            "generation": op.generation,
            "report": op.report,
        }))
        .into_response(),
        Ok(None) => err(StatusCode::NOT_FOUND, "scan_operation.missing", "扫描操作不存在"),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &e.to_string()),
    }
}

pub(crate) async fn cancel_scan_operation(
    State(state): State<ApiState>,
    Path((_lib_id, op_id)): Path<(String, String)>,
) -> Response {
    let now_ms = crate::job_loop::unix_now() * 1000;
    let store = state.store.lock();
    match store.cancel_scan(&op_id, now_ms) {
        Ok(true) => ok(json!({ "cancelled": true })).into_response(),
        Ok(false) => err(StatusCode::NOT_FOUND, "scan_operation.missing", "未找到可取消的扫描操作"),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &e.to_string()),
    }
}
