//! ScanCoordinator orchestrating single-flight durable scan operations.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use store::{ScanOperation, ScanOperationStatus};

use crate::management::ApiState;

pub struct ScanCoordinator {
    running_count: Mutex<usize>,
}

impl ScanCoordinator {
    pub fn new() -> Self {
        Self {
            running_count: Mutex::new(0),
        }
    }

    /// Enqueue a scan operation. If an active operation already exists for this library, returns it.
    pub fn enqueue(
        &self,
        state: &ApiState,
        library_id: &str,
        targets: Vec<PathBuf>,
    ) -> Result<ScanOperation, String> {
        let signature = {
            let store = state.store.lock();
            let lib = store
                .get_library(library_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "Library missing".to_string())?;
            format!("{}:{}", lib.kind.as_str(), lib.root_paths.len())
        };

        let now_ms = crate::job_loop::unix_now() * 1000;
        let op = {
            let store = state.store.lock();
            store
                .enqueue_scan(library_id, &signature, &targets, now_ms)
                .map_err(|e| e.to_string())?
        };

        // If operation is already running, return it
        if op.status == ScanOperationStatus::Running {
            return Ok(op);
        }

        // Spawn async execution worker if within concurrency limit (max 2 global)
        let state_clone = state.clone();
        let op_id = op.id.clone();
        let lib_id = library_id.to_string();
        tokio::spawn(async move {
            run_operation(&state_clone, &op_id, &lib_id).await;
        });

        Ok(op)
    }
}

async fn run_operation(state: &ApiState, op_id: &str, library_id: &str) {
    let lease = match state.acquire_library_work(library_id) {
        Ok(lease) => lease,
        Err(e) => {
            let store = state.store.lock();
            let _ = store.update_scan_operation_status(
                op_id,
                ScanOperationStatus::Failed,
                &serde_json::json!({ "error": e }),
            );
            return;
        }
    };

    {
        let store = state.store.lock();
        let _ = store.update_scan_operation_status(
            op_id,
            ScanOperationStatus::Running,
            &serde_json::json!({}),
        );
    }

    let state_clone = state.clone();
    let lib_id = library_id.to_string();
    let scan_res = tokio::task::spawn_blocking(move || {
        crate::http::library_scan::scan_library_leased(&state_clone, &lib_id, &lease)
    })
    .await;

    let store = state.store.lock();
    match scan_res {
        Ok(resp) => {
            let status = if resp.status().is_success() {
                ScanOperationStatus::Completed
            } else {
                ScanOperationStatus::Failed
            };
            let _ = store.update_scan_operation_status(
                op_id,
                status,
                &serde_json::json!({ "http_status": resp.status().as_u16() }),
            );
        }
        Err(e) => {
            let _ = store.update_scan_operation_status(
                op_id,
                ScanOperationStatus::Failed,
                &serde_json::json!({ "error": e.to_string() }),
            );
        }
    }
}
