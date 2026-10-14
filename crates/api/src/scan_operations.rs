//! ScanCoordinator orchestrating single-flight durable scan operations.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use parking_lot::Mutex;
use store::{ScanOperation, ScanOperationStatus};

use crate::management::ApiState;

#[derive(Debug)]
pub enum ScanEnqueueError {
    NotFound(String),
    Conflict(String),
    Internal(String),
}

pub struct ScanCoordinator {
    running_ops: Mutex<HashSet<String>>,
    active_controls: Mutex<HashMap<String, library::WorkControl>>,
}

impl ScanCoordinator {
    pub fn new() -> Self {
        Self {
            running_ops: Mutex::new(HashSet::new()),
            active_controls: Mutex::new(HashMap::new()),
        }
    }

    pub fn register_control(&self, op_id: &str, control: library::WorkControl) {
        self.active_controls.lock().insert(op_id.to_string(), control);
    }

    pub fn unregister_control(&self, op_id: &str) {
        self.active_controls.lock().remove(op_id);
    }

    pub fn cancel_operation(&self, op_id: &str) {
        if let Some(control) = self.active_controls.lock().get(op_id) {
            control.cancel();
        }
    }

    pub fn trigger_next(&self, state: &ApiState) {
        let mut running = self.running_ops.lock();
        if running.len() >= 2 {
            return;
        }
        let excluding: Vec<String> = running.iter().cloned().collect();
        let store = state.store.lock();
        let op_opt = match store.claim_next_queued_scan(&excluding) {
            Ok(op) => op,
            Err(e) => {
                tracing::error!(%e, "Failed to claim next queued scan operation");
                None
            }
        };
        drop(store);

        if let Some(op) = op_opt {
            running.insert(op.id.clone());
            drop(running);
            let state_clone = state.clone();
            tokio::spawn(async move {
                run_operation(&state_clone, &op.id, &op.library_id).await;
                state_clone.scan_coordinator.running_ops.lock().remove(&op.id);
                state_clone.scan_coordinator.trigger_next(&state_clone);
            });
        }
    }

    /// Enqueue a scan operation. If an active operation already exists for this library, returns it.
    pub fn enqueue(
        &self,
        state: &ApiState,
        library_id: &str,
        targets: Vec<PathBuf>,
    ) -> Result<ScanOperation, ScanEnqueueError> {
        let signature = {
            let store = state.store.lock();
            let lib = store
                .get_library(library_id)
                .map_err(|e| ScanEnqueueError::Internal(e.to_string()))?
                .ok_or_else(|| ScanEnqueueError::NotFound("Library 不存在".to_string()))?;
            if !store
                .library_accepts_work(library_id)
                .map_err(|e| ScanEnqueueError::Internal(e.to_string()))?
            {
                return Err(ScanEnqueueError::Conflict("Library 正在删除或已冻结".to_string()));
            }
            let mut roots: Vec<_> = lib.root_paths.iter().map(|p| p.display().to_string()).collect();
            roots.sort();
            format!("{}:{}:{}", lib.kind.as_str(), lib.root_paths.len(), roots.join(","))
        };

        if state.acquire_library_work(library_id).is_err() {
            return Err(ScanEnqueueError::Conflict("Library 正在删除或已冻结".to_string()));
        }

        let now_ms = crate::job_loop::unix_now() * 1000;
        let op = {
            let store = state.store.lock();
            store
                .enqueue_scan(library_id, &signature, &targets, now_ms)
                .map_err(|e| ScanEnqueueError::Internal(e.to_string()))?
        };

        // If operation is already running or being executed by a worker, return it
        {
            let running = self.running_ops.lock();
            if running.contains(&op.id) || op.status == ScanOperationStatus::Running {
                return Ok(op);
            }
        }

        self.trigger_next(state);

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

    state.scan_coordinator.register_control(op_id, lease.control().clone());

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
        crate::http::library_scan::scan_library_leased_result(&state_clone, &lib_id, &lease)
    })
    .await;

    state.scan_coordinator.unregister_control(op_id);

    let store = state.store.lock();
    match scan_res {
        Ok(Ok((scan_report, report_data))) => {
            let any_partial_or_unreachable = scan_report
                .roots
                .iter()
                .any(|r| matches!(r.status, library::ScanRootStatus::Partial | library::ScanRootStatus::Unreachable));
            let status = if any_partial_or_unreachable || scan_report.error_count > 0 {
                ScanOperationStatus::Partial
            } else {
                ScanOperationStatus::Completed
            };
            let _ = store.update_scan_operation_status(op_id, status, &report_data);
        }
        Ok(Err(resp)) => {
            let status = ScanOperationStatus::Failed;
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
