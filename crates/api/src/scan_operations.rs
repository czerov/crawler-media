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
                // 即使 worker panic，也要释放 running 槽位，否则两次 panic 之后
                // 全局 2 个扫描额度会被永久占满。
                let guard = RunningGuard {
                    coordinator: state_clone.scan_coordinator.clone(),
                    op_id: op.id.clone(),
                    released: false,
                };
                run_operation(&state_clone, &op.id, &op.library_id).await;
                guard.release();
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

/// 释放 running 槽位的 RAII guard：worker 正常结束或 panic 都会移除 op id。
struct RunningGuard {
    coordinator: std::sync::Arc<ScanCoordinator>,
    op_id: String,
    released: bool,
}

impl RunningGuard {
    fn release(mut self) {
        self.released = true;
        self.coordinator.running_ops.lock().remove(&self.op_id);
    }
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.coordinator.running_ops.lock().remove(&self.op_id);
        if !self.released && std::thread::panicking() {
            tracing::error!(
                op_id = %self.op_id,
                "扫描 worker 异常退出；operation 保持 running，等待下次启动恢复"
            );
        }
    }
}

/// 写终态；写失败会让 operation 永远停在 running，所以必须留痕。
fn write_terminal_status(
    store: &crate::Store,
    op_id: &str,
    status: ScanOperationStatus,
    report: &serde_json::Value,
) {
    match store.update_scan_operation_status(op_id, status, report) {
        Ok(true) => {
            if let Err(error) = store.clear_scan_seen(op_id) {
                tracing::error!(%error, %op_id, "清理 scan_seen 检查点失败");
            }
        }
        Ok(false) => {
            tracing::warn!(%op_id, status = status.as_str(), "扫描终态未写入：operation 已是终态");
        }
        Err(error) => {
            tracing::error!(%error, %op_id, status = status.as_str(), "写入扫描终态失败");
        }
    }
}

async fn run_operation(state: &ApiState, op_id: &str, library_id: &str) {
    let lease = match state.acquire_library_work(library_id) {
        Ok(lease) => lease,
        Err(e) => {
            let store = state.store.lock();
            write_terminal_status(
                &store,
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
        if let Err(error) = store.update_scan_operation_status(
            op_id,
            ScanOperationStatus::Running,
            &serde_json::json!({}),
        ) {
            tracing::error!(%error, %op_id, "标记扫描为 running 失败");
        }
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
            write_terminal_status(&store, op_id, status, &report_data);
        }
        Ok(Err(resp)) => {
            write_terminal_status(
                &store,
                op_id,
                ScanOperationStatus::Failed,
                &serde_json::json!({ "http_status": resp.status().as_u16() }),
            );
        }
        Err(e) => {
            write_terminal_status(
                &store,
                op_id,
                ScanOperationStatus::Failed,
                &serde_json::json!({ "error": e.to_string() }),
            );
        }
    }
}
