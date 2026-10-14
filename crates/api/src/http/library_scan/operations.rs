//! HTTP handlers for scan operation polling and cancellation.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::http::{err, ok};
use crate::management::ApiState;

pub(crate) async fn get_scan_operation(
    State(state): State<ApiState>,
    Path((lib_id, op_id)): Path<(String, String)>,
) -> Response {
    let store = state.store.lock();
    match store.scan_operation(&op_id) {
        Ok(Some(op)) if op.library_id == lib_id => ok(json!({
            "operation_id": op.id,
            "status": op.status.as_str(),
            "generation": op.generation,
            "report": op.report,
        }))
        .into_response(),
        Ok(Some(_)) | Ok(None) => err(StatusCode::NOT_FOUND, "scan_operation.missing", "扫描操作不存在"),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &e.to_string()),
    }
}

pub(crate) async fn cancel_scan_operation(
    State(state): State<ApiState>,
    Path((lib_id, op_id)): Path<(String, String)>,
) -> Response {
    let now_ms = crate::job_loop::unix_now() * 1000;
    let cancel_res = {
        let store = state.store.lock();
        let op_opt = match store.scan_operation(&op_id) {
            Ok(opt) => opt,
            Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &e.to_string()),
        };
        let Some(op) = op_opt else {
            return err(StatusCode::NOT_FOUND, "scan_operation.missing", "未找到可取消的扫描操作");
        };
        if op.library_id != lib_id {
            return err(StatusCode::NOT_FOUND, "scan_operation.missing", "未找到可取消的扫描操作");
        }
        store.cancel_scan(&op_id, now_ms)
    };

    match cancel_res {
        Ok(true) => {
            state.scan_coordinator.cancel_operation(&op_id);
            ok(json!({ "cancelled": true })).into_response()
        }
        Ok(false) => err(StatusCode::NOT_FOUND, "scan_operation.missing", "未找到可取消的扫描操作"),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &e.to_string()),
    }
}
