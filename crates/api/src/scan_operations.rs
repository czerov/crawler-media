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

/// 扫描 scope 的持久化指纹：Library 种类 + 根数量 + 排序后的根路径。
///
/// 入队时写入 `scan_operations.scope_signature`，执行/恢复前用它做准入比对：
/// 配置变过的旧 operation 必须取消，而不是拿旧 scope 继续扫（B3 design:99）。
///
/// 编码必须无歧义：根路径长度前缀 + 分隔符，逗号只是路径里的普通字符，
/// `["/x/a,/x/b","/x/c"]` 与 `["/x/a","/x/b,/x/c"]` 不能碰撞（P2-4）。
pub fn scan_scope_signature(lib: &store::Library) -> String {
    let mut roots: Vec<_> = lib.root_paths.iter().map(|p| p.display().to_string()).collect();
    roots.sort();
    let mut signature = format!("v2:{}:{}", lib.kind.as_str(), roots.len());
    for root in roots {
        signature.push(':');
        signature.push_str(&root.len().to_string());
        signature.push(':');
        signature.push_str(&root);
    }
    signature
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
            let targets = op.targets.clone();
            let full_scope = op.is_full_scope();
            // 本轮消费的 dirty 版本取自认领时的行：之后并入的新事件会让 CAS 失败并补扫。
            let expected_dirty_rev = op.dirty_rev;
            tokio::spawn(async move {
                // 无论 worker 正常结束还是 panic，都会释放 running 槽位并唤醒下一个
                // 排队 operation，避免两次 panic 就把全局 2 个扫描额度永久占满。
                let _guard = RunningGuard {
                    state: state_clone.clone(),
                    op_id: op.id.clone(),
                };
                run_operation(
                    &state_clone,
                    &op.id,
                    &op.library_id,
                    &targets,
                    full_scope,
                    expected_dirty_rev,
                )
                .await;
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
            scan_scope_signature(&lib)
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
/// worker 结束（正常或 panic）都释放 running 槽位，并唤醒下一个排队 operation。
struct RunningGuard {
    state: ApiState,
    op_id: String,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.state
            .scan_coordinator
            .running_ops
            .lock()
            .remove(&self.op_id);
        if std::thread::panicking() {
            tracing::error!(
                op_id = %self.op_id,
                "扫描 worker 异常退出；本次 operation 未写终态，槽位已释放并继续排队"
            );
        }
        self.state.scan_coordinator.trigger_next(&self.state);
    }
}

/// 写终态（CAS）：只有本轮消费的 `dirty_rev` 仍是当前版本才落终态；否则补扫重排（P3-1）。
///
/// 判定与写入在 store 的同一个 IMMEDIATE 事务里完成，另一连接在判定后、写入前并入的
/// 事件不会被覆盖。返回 `true` 表示 operation 已终态（或已被取消），本轮结束。
fn finalize_scan(
    store: &crate::Store,
    op_id: &str,
    expected_dirty_rev: i64,
    status: ScanOperationStatus,
    report: &serde_json::Value,
) -> bool {
    match store.finalize_or_requeue_scan(op_id, expected_dirty_rev, status, report) {
        Ok(Some(consumed_rev)) => {
            tracing::info!(
                %op_id,
                consumed_rev,
                "扫描期间并入新的请求，累计报告已保留并回到队列补扫一轮"
            );
            false
        }
        Ok(None) => true,
        Err(error) => {
            tracing::error!(%error, %op_id, status = status.as_str(), "写入扫描终态失败");
            false
        }
    }
}

/// 执行准入读到的持久化状态：累计报告 + scope 是否已过期。
struct ScanAdmission {
    base_report: serde_json::Value,
    stale_reason: Option<String>,
}

/// 执行准入：持久化的 scope signature 与当前 scope 不一致时返回取消原因。
///
/// 读失败（库不存在/查询出错）不在这里取消：让后续的 `acquire_library_work` 与
/// `ScanScope::validate` 按既有语义处理，避免把瞬时读错误当成配置变化。
fn scan_admission(state: &ApiState, op_id: &str, library_id: &str) -> Option<ScanAdmission> {
    let store = state.store.lock();
    let op = match store.scan_operation(op_id) {
        Ok(Some(op)) => op,
        Ok(None) => return None,
        Err(error) => {
            tracing::error!(%error, %op_id, "读取扫描 operation 失败，跳过 scope 准入复核");
            return None;
        }
    };
    let base_report = op.report.clone();
    let lib = match store.get_library(library_id) {
        Ok(Some(lib)) => lib,
        Ok(None) => return None,
        Err(error) => {
            tracing::error!(%error, %library_id, %op_id, "读取 Library 失败，跳过 scope 准入复核");
            return None;
        }
    };
    let current = scan_scope_signature(&lib);
    let stale_reason = (current != op.scope_signature).then(|| {
        format!(
            "library scope changed: persisted={} current={}",
            op.scope_signature, current
        )
    });
    Some(ScanAdmission {
        base_report,
        stale_reason,
    })
}

/// 取消过期 operation：写 cancelled 并清理断点，零登记。
fn cancel_stale_operation(store: &crate::Store, op_id: &str, reason: &str) {
    if let Err(error) = store.update_scan_operation_status(
        op_id,
        ScanOperationStatus::Cancelled,
        &serde_json::json!({ "error": "library.scope_changed", "reason": reason }),
    ) {
        tracing::error!(%error, %op_id, "取消过期扫描 operation 失败");
    }
    if let Err(error) = store.clear_scan_seen(op_id) {
        tracing::error!(%error, %op_id, "清理过期扫描断点失败");
    }
}

async fn run_operation(
    state: &ApiState,
    op_id: &str,
    library_id: &str,
    targets: &[PathBuf],
    full_scope: bool,
    expected_dirty_rev: i64,
) {
    // B3 design:99：入队时写下的 scope 与当前 scope 不一致，就取消而不是照旧执行。
    let Some(admission) = scan_admission(state, op_id, library_id) else {
        return;
    };
    if let Some(reason) = admission.stale_reason {
        tracing::warn!(%op_id, %library_id, %reason, "扫描 scope 已变化，取消过期 operation");
        let store = state.store.lock();
        cancel_stale_operation(&store, op_id, &reason);
        return;
    }
    let base_report = admission.base_report;

    let lease = match state.acquire_library_work(library_id) {
        Ok(lease) => lease,
        Err(e) => {
            let store = state.store.lock();
            let report = with_error(merge_scan_reports(&base_report, &serde_json::json!({})), &e);
            finalize_scan(&store, op_id, expected_dirty_rev, ScanOperationStatus::Failed, &report);
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
    let scan_targets = targets.to_vec();
    let op_id_owned = op_id.to_string();
    let scan_res = tokio::task::spawn_blocking(move || {
        // op_id 同时是 scan_seen 断点的归属：中断重排后同一 operation 从断点续扫。
        crate::http::library_scan::scan_library_leased_resumable(
            &state_clone,
            &lib_id,
            &lease,
            &scan_targets,
            full_scope,
            Some(&op_id_owned),
        )
    })
    .await;

    state.scan_coordinator.unregister_control(op_id);

    let store = state.store.lock();
    match scan_res {
        Ok(Ok((_scan_report, report_data))) => {
            // 累计本轮报告：补扫轮不能丢掉第一轮已提交的计数（P3-3）。
            let report = merge_scan_reports(&base_report, &report_data);
            // 终态取累计报告的 status：前几轮的 partial/unreachable 不能被后续干净轮洗掉。
            let status = if report.get("status").and_then(serde_json::Value::as_str)
                == Some("partial")
            {
                ScanOperationStatus::Partial
            } else {
                ScanOperationStatus::Completed
            };
            finalize_scan(&store, op_id, expected_dirty_rev, status, &report);
        }
        Ok(Err(crate::http::library_scan::runner::ScanRunError::ScopeChanged { reason })) => {
            // 捕获 scope 复核失败发生在发布任何 chunk 之前：零登记。
            tracing::warn!(%op_id, %library_id, %reason, "捕获 scope 已变化，取消过期 operation");
            cancel_stale_operation(&store, op_id, &reason);
        }
        Ok(Err(crate::http::library_scan::runner::ScanRunError::Failed(failure))) => {
            // 失败终态：已提交计数 + 错误摘要，另外保留 HTTP 状态便于排查。
            let http_status = failure.response.status().as_u16();
            let mut report =
                with_error(merge_scan_reports(&base_report, &failure.report), &failure.error);
            report["http_status"] = serde_json::json!(http_status);
            finalize_scan(&store, op_id, expected_dirty_rev, ScanOperationStatus::Failed, &report);
        }
        Err(e) => {
            let report = with_error(
                merge_scan_reports(&base_report, &serde_json::json!({})),
                &e.to_string(),
            );
            finalize_scan(&store, op_id, expected_dirty_rev, ScanOperationStatus::Failed, &report);
        }
    }
}

fn is_nonempty_object(value: &serde_json::Value) -> bool {
    value.as_object().is_some_and(|map| !map.is_empty())
}

fn sum_report_field(base: &serde_json::Value, round: &serde_json::Value, key: &str) -> u64 {
    base.get(key).and_then(serde_json::Value::as_u64).unwrap_or(0)
        + round.get(key).and_then(serde_json::Value::as_u64).unwrap_or(0)
}

fn worst_root_status(a: Option<&str>, b: Option<&str>) -> &'static str {
    let rank = |status: Option<&str>| match status {
        Some("unreachable") => 2,
        Some("partial") => 1,
        _ => 0,
    };
    match rank(a).max(rank(b)) {
        2 => "unreachable",
        1 => "partial",
        _ => "completed",
    }
}

/// 按路径合并两轮的根报告：visited/errors 相加，status 取更差的一侧。
fn merge_report_roots(base: &serde_json::Value, round: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut merged: Vec<serde_json::Value> = Vec::new();
    for source in [base, round] {
        for root in source
            .get("roots")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let path = root
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            match merged
                .iter_mut()
                .find(|row| row.get("path").and_then(serde_json::Value::as_str) == Some(path))
            {
                Some(existing) => {
                    existing["visited"] =
                        serde_json::json!(sum_report_field(existing, root, "visited"));
                    existing["errors"] =
                        serde_json::json!(sum_report_field(existing, root, "errors"));
                    existing["status"] = serde_json::json!(worst_root_status(
                        existing.get("status").and_then(serde_json::Value::as_str),
                        root.get("status").and_then(serde_json::Value::as_str),
                    ));
                }
                None => merged.push(root.clone()),
            }
        }
    }
    merged
}

/// 跨轮累计：把本轮报告并入已持久化的累计报告（P3-3）。
///
/// 计数相加、roots 按路径合并、issues 截断到 20 条；status 由合并结果重算。
fn merge_scan_reports(
    base: &serde_json::Value,
    round: &serde_json::Value,
) -> serde_json::Value {
    if !is_nonempty_object(base) {
        return round.clone();
    }
    if !is_nonempty_object(round) {
        return base.clone();
    }
    let roots = merge_report_roots(base, round);
    let issues: Vec<serde_json::Value> = [base, round]
        .into_iter()
        .flat_map(|source| {
            source
                .get("issues")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .take(20)
        .collect();
    let error_count = sum_report_field(base, round, "error_count");
    let any_bad_root = roots.iter().any(|root| {
        matches!(
            root.get("status").and_then(serde_json::Value::as_str),
            Some("partial") | Some("unreachable")
        )
    });
    serde_json::json!({
        "queued": true,
        "transferred": true,
        "status": if any_bad_root || error_count > 0 { "partial" } else { "completed" },
        "scanned": sum_report_field(base, round, "scanned"),
        "inserted": sum_report_field(base, round, "inserted"),
        "restored": sum_report_field(base, round, "restored"),
        "probe_pending": sum_report_field(base, round, "probe_pending"),
        "roots": roots,
        "error_count": error_count,
        "errors_truncated": error_count > 20,
        "issues": issues,
    })
}

/// 失败终态：保留累计计数，并写入错误摘要（P3-3）。
fn with_error(mut report: serde_json::Value, error: &str) -> serde_json::Value {
    if !report.is_object() {
        report = serde_json::json!({});
    }
    report["status"] = serde_json::json!("failed");
    report["error"] = serde_json::json!(error);
    report
}


#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
    use store::ScanOperation;

    use super::*;

    struct NoFetch;

    impl indexer::Fetcher for NoFetch {
        fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
            Err(indexer::IndexerError::Fetch("offline".into()))
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        state: ApiState,
        library_id: String,
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = crate::Store::open(dir.path().join("data")).unwrap();
            let state = ApiState::new(
                store,
                "test-token".into(),
                indexer::ProfileSet::load(None).unwrap(),
                Arc::new(NoFetch),
                Arc::new(downloader::MemoryDownloader::new(dir.path().join("stage"))),
                dir.path().join("library"),
            )
            .unwrap();
            let root = dir.path().join("movies");
            std::fs::create_dir_all(&root).unwrap();
            let library_id = {
                let store = state.store();
                let guard = store.lock();
                guard
                    .create_library(
                        MediaKind::Movie,
                        "Movies",
                        &[root.to_str().unwrap()],
                        "everyone",
                        true,
                        &[],
                    )
                    .unwrap()
                    .id
            };
            Self {
                _dir: dir,
                state,
                library_id,
                root,
            }
        }

        fn signature(&self) -> String {
            let store = self.state.store();
            let guard = store.lock();
            let library = guard.get_library(&self.library_id).unwrap().unwrap();
            scan_scope_signature(&library)
        }

        fn insert_media_and_ledger(&self, rel_path: &str) -> (MediaId, LedgerRow) {
            let media = Media {
                id: MediaId::new(),
                kind: MediaKind::Movie,
                title: "Test Movie".to_string(),
                year: None,
                original_title: None,
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            };
            let row = LedgerRow {
                id: LedgerId::new(),
                media_id: media.id,
                path: self.root.join(rel_path).to_str().unwrap().to_string(),
                season: None,
                episode: None,
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
            };
            let store = self.state.store();
            let guard = store.lock();
            guard.insert_media(&media).unwrap();
            guard.insert_ledger(&row).unwrap();
            (media.id, row)
        }

        async fn wait_terminal(&self, op_id: &str) -> ScanOperation {
            for _ in 0..600 {
                let op = self.state.store().lock().scan_operation(op_id).unwrap();
                if let Some(op) = op
                    && !op.status.is_active()
                {
                    return op;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            panic!("scan operation {op_id} 未在预期时间内进入终态");
        }
    }

    /// P2-4：scope 指纹必须无歧义——根路径里的逗号不能造成不同配置碰撞。
    #[test]
    fn scope_signature_does_not_collide_on_commas_in_roots() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::Store::open(dir.path().join("data")).unwrap();
        let a = store
            .create_library(
                MediaKind::Movie,
                "A",
                &["/x/a,/x/b", "/x/c"],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let b = store
            .create_library(
                MediaKind::Movie,
                "B",
                &["/x/a", "/x/b,/x/c"],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        assert_ne!(
            scan_scope_signature(&a),
            scan_scope_signature(&b),
            "逗号拼接会让不同根配置碰撞: {}",
            scan_scope_signature(&a)
        );
        // kind 也必须参与编码。
        let tv = store
            .create_library(
                MediaKind::Tv,
                "TV",
                &["/x/a,/x/b", "/x/c"],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        assert_ne!(scan_scope_signature(&a), scan_scope_signature(&tv));
    }

    /// P3-1 + P3-3：轮内并入的事件必须让本轮补扫，且终态报告是跨轮累计。
    #[tokio::test]
    async fn dirty_event_during_round_requeues_with_cumulative_report() {
        let fix = Fixture::new();
        let dir = fix.root.join("Show A");
        std::fs::create_dir_all(&dir).unwrap();
        let f1 = dir.join("Movie1 (2024).mkv");
        std::fs::write(&f1, b"1").unwrap();
        fix.insert_media_and_ledger("Show A/Movie1 (2024).mkv");
        let sig = fix.signature();
        let op = {
            let store = fix.state.store();
            let guard = store.lock();
            let op = guard
                .enqueue_scan(&fix.library_id, &sig, &[dir.clone()], 1000)
                .unwrap();
            guard.record_scan_seen(&op.id, &[f1.clone()]).unwrap();
            op
        };
        let f2 = dir.join("Movie2 (2024).mkv");
        std::fs::write(&f2, b"2").unwrap();

        // 屏障：本轮已经开始（认领后），此时并入同一 target 的第二个事件。
        let state = fix.state.clone();
        let library_id = fix.library_id.clone();
        let target = dir.clone();
        let hook_sig = sig.clone();
        let hook_op_id = op.id.clone();
        crate::http::library_scan::runner::test_scope_hook::set(
            &hook_op_id,
            Box::new(move || {
                let store = state.store();
                let guard = store.lock();
                guard
                    .enqueue_scan(&library_id, &hook_sig, &[target.clone()], 1001)
                    .unwrap();
            }),
        );

        fix.state.scan_coordinator.trigger_next(&fix.state);
        let done = fix.wait_terminal(&op.id).await;
        assert_eq!(done.status, ScanOperationStatus::Completed, "{:?}", done.report);
        assert_eq!(done.dirty_rev, 1);
        assert_eq!(done.consumed_rev, 1, "轮内事件必须被补扫轮消费");
        assert_eq!(
            done.report["scanned"], 1,
            "累计报告必须包含第一轮已处理的文件: {}",
            done.report
        );
        assert_eq!(done.report["inserted"], 1, "{}", done.report);
        let rows = fix.state.store().lock().list_ledger().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .any(|row| row.path.ends_with("Movie2 (2024).mkv")));
    }

    /// P2-4：准入通过之后、scope 捕获之前 PATCH roots，旧 operation 必须取消且零登记。
    #[tokio::test]
    async fn scope_change_between_admission_and_capture_cancels_without_ledgering() {
        let fix = Fixture::new();
        let file = fix.root.join("Movie1 (2024).mkv");
        std::fs::write(&file, b"1").unwrap();
        let sig = fix.signature();
        let op = {
            let store = fix.state.store();
            let guard = store.lock();
            guard
                .enqueue_scan(&fix.library_id, &sig, &[], 1000)
                .unwrap()
        };

        // 屏障：准入已通过，但在捕获 scope 之前把 roots 换到别处。
        let moved_root = fix.root.parent().unwrap().join("movies-moved");
        std::fs::create_dir_all(&moved_root).unwrap();
        let state = fix.state.clone();
        let library_id = fix.library_id.clone();
        let moved = moved_root.to_str().unwrap().to_string();
        let hook_op_id = op.id.clone();
        crate::http::library_scan::runner::test_scope_hook::set(
            &hook_op_id,
            Box::new(move || {
                let store = state.store();
                let guard = store.lock();
                guard
                    .update_library(&library_id, None, Some(&[moved.as_str()]), None)
                    .unwrap();
            }),
        );

        fix.state.scan_coordinator.trigger_next(&fix.state);
        let done = fix.wait_terminal(&op.id).await;
        assert_eq!(
            done.status,
            ScanOperationStatus::Cancelled,
            "{:?}",
            done.report
        );
        assert_eq!(done.report["error"], "library.scope_changed");
        assert!(
            done.report["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("captured scope changed")),
            "取消原因必须来自捕获 scope 复核: {}",
            done.report
        );
        assert!(
            fix.state.store().lock().list_ledger().unwrap().is_empty(),
            "取消的 operation 不得登记任何文件"
        );
    }

    /// P2-NEW-2：文件版本观察（模拟慢 NAS stat）期间不得持有 Store 锁。
    ///
    /// 屏障阻塞在观察点；测试线程必须仍能立刻拿到 Store 锁，否则说明慢观察把共享
    /// Store 拖住了（auth 等其他请求会被阻塞）。
    #[tokio::test]
    async fn slow_file_observation_keeps_the_store_lock_free() {
        let fix = Fixture::new();
        let file = fix.root.join("Movie1 (2024).mkv");
        std::fs::write(&file, b"1").unwrap();
        let sig = fix.signature();
        let op = {
            let store = fix.state.store();
            let guard = store.lock();
            guard
                .enqueue_scan(&fix.library_id, &sig, &[], 1000)
                .unwrap()
        };

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let hook_op_id = op.id.clone();
        crate::http::library_scan::runner::test_observe_hook::set(
            &hook_op_id,
            Box::new(move || {
                let _ = entered_tx.send(());
                // 模拟慢 NAS stat：观察窗口内一直阻塞。
                let _ = release_rx.recv();
            }),
        );

        fix.state.scan_coordinator.trigger_next(&fix.state);
        entered_rx.await.expect("worker 必须进入慢观察屏障");

        let store = fix.state.store();
        let guard = store
            .try_lock_for(std::time::Duration::from_secs(2))
            .expect("慢文件观察期间 Store 锁必须可用（P2-NEW-2）");
        assert!(
            guard.scan_operation(&op.id).unwrap().is_some(),
            "慢观察期间其他 Store 使用者必须仍能读"
        );
        drop(guard);
        release_tx.send(()).unwrap();

        let done = fix.wait_terminal(&op.id).await;
        assert_eq!(done.status, ScanOperationStatus::Completed, "{:?}", done.report);
        assert_eq!(done.report["scanned"], 1, "{}", done.report);
    }
}
