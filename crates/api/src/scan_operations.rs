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
///
/// 旧构建写下的 `v1` 编码（`kind:count:root1,root2,...`）由
/// [`match_legacy_scope_signature`] 在准入时识别：只有与「当前配置重新编码出的旧格式」
/// 逐字节相同的旧签名才可就地升级；有歧义或与当前配置不同的旧签名一律取消（P2-NEW-1）。
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

/// 执行准入读到的持久化状态：累计报告 + scope 是否已过期 + 签名升级原因。
struct ScanAdmission {
    base_report: serde_json::Value,
    stale_reason: Option<String>,
    /// 旧格式签名已在本次准入升级时的显式原因（P2-NEW-1），写进终态报告。
    signature_upgrade: Option<String>,
}

/// 旧（v1）scope 签名的编码：`kind:count:root1,root2,...`（排序后逗号拼接）。
///
/// 只用于把**当前**配置重新编码成旧格式，与持久化的旧签名做逐字节等价性证明。
/// 绝不能反过来用 `split(',')` 反解旧签名里的根路径：根路径本身含逗号时该编码不可
/// 唯一还原，反解会把「配置已变」误判成「只是格式不同」（P2-NEW-1）。
fn legacy_scope_signature(library: &store::Library) -> String {
    let mut roots: Vec<String> = library
        .root_paths
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    roots.sort();
    format!(
        "{}:{}:{}",
        library.kind.as_str(),
        roots.len(),
        roots.join(",")
    )
}

/// 旧（v1）scope 签名与当前配置的比对结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LegacySignatureMatch {
    /// 与「当前配置重新编码出的旧格式」逐字节相同：这行确实来自同一配置，可就地升级。
    Same,
    /// 旧编码不可唯一还原（根路径含分隔符），且与当前配置的旧编码不同：无法证明等价，
    /// 必须取消。理由字符串只是留痕，不能替代这个安全判定（P2-NEW-1）。
    Ambiguous,
    /// 可无歧义解码，但与当前配置不一致：真实的 scope 变化，必须取消。
    Different,
}

/// 解析旧格式签名 `kind:count:root1,root2,...` 并与当前 Library 配置比对。
///
/// 返回 `None` = 不是可识别的旧格式（空串、损坏值、已是新格式），调用方按 scope
/// 变化处理。
///
/// 安全判据只有一个：把**当前**配置重新编码成旧格式后，是否与持久化值逐字节相同。
/// 只有 [`LegacySignatureMatch::Same`] 允许就地升级；`Ambiguous` 与 `Different` 都必须
/// 取消——「旧编码有歧义」不是放行理由，恰恰是无法证明等价的证据。
fn match_legacy_scope_signature(
    persisted: &str,
    library: &store::Library,
) -> Option<LegacySignatureMatch> {
    if persisted.starts_with("v2:") {
        return None;
    }
    if persisted == legacy_scope_signature(library) {
        return Some(LegacySignatureMatch::Same);
    }
    let mut parts = persisted.splitn(3, ':');
    let kind = parts.next()?;
    let count: usize = parts.next()?.parse().ok()?;
    let joined = parts.next()?;
    if kind != library.kind.as_str() {
        return Some(LegacySignatureMatch::Different);
    }
    let segments = if joined.is_empty() {
        0
    } else {
        joined.split(',').count()
    };
    Some(if segments == count {
        LegacySignatureMatch::Different
    } else {
        LegacySignatureMatch::Ambiguous
    })
}

fn stale_scope_reason(persisted: &str, current: &str) -> String {
    format!("library scope changed: persisted={persisted} current={current}")
}

/// 旧编码有歧义且与当前配置的旧编码不同：无法证明等价，按 scope 变化取消。
fn unprovable_legacy_reason(persisted: &str, current: &str) -> String {
    format!(
        "legacy scope signature is ambiguous and not provably equivalent to current config: \
         persisted={persisted} current={current}"
    )
}

/// 准入签名判定：`Ok(Some(reason))` = 旧格式已升级；`Ok(None)` = 与新编码一致；
/// `Err(reason)` = 真实的 scope 变化，调用方必须取消。
///
/// 只有「当前配置重新编码成旧格式后与持久化值逐字节相同」才允许升级（P2-NEW-1）；
/// 有歧义或可解码但不一致的旧签名一律取消，绝不升级成当前配置去扫新 scope。
///
/// 升级走 store 的 CAS（同一 IMMEDIATE 事务内比对+写入）；CAS 失败说明签名被并发
/// 改写，重读一次再判定，绝不覆盖别人的值。
fn reconcile_scan_scope_signature(
    store: &crate::Store,
    op: &ScanOperation,
    library: &store::Library,
) -> Result<Option<String>, String> {
    let current = scan_scope_signature(library);
    if current == op.scope_signature {
        return Ok(None);
    }
    let legacy = match_legacy_scope_signature(&op.scope_signature, library);
    if legacy != Some(LegacySignatureMatch::Same) {
        return Err(match legacy {
            Some(LegacySignatureMatch::Ambiguous) => {
                unprovable_legacy_reason(&op.scope_signature, &current)
            }
            _ => stale_scope_reason(&op.scope_signature, &current),
        });
    }
    let reason = format!(
        "signature format upgraded: persisted={} current={}",
        op.scope_signature, current
    );
    match store.upgrade_scan_scope_signature(&op.id, &op.scope_signature, &current) {
        Ok(true) => {
            tracing::warn!(op_id = %op.id, %reason, "扫描 scope 签名已升级，保留扫描意图");
            Ok(Some(reason))
        }
        Ok(false) => match store.scan_operation(&op.id).ok().flatten() {
            Some(fresh) if fresh.scope_signature == current => Ok(Some(reason)),
            _ => Err(stale_scope_reason(&op.scope_signature, &current)),
        },
        Err(error) => {
            tracing::error!(%error, op_id = %op.id, "升级扫描 scope 签名失败，按 scope 变化处理");
            Err(stale_scope_reason(&op.scope_signature, &current))
        }
    }
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
    match reconcile_scan_scope_signature(&store, &op, &lib) {
        Ok(signature_upgrade) => Some(ScanAdmission {
            base_report,
            stale_reason: None,
            signature_upgrade,
        }),
        Err(stale_reason) => Some(ScanAdmission {
            base_report,
            stale_reason: Some(stale_reason),
            signature_upgrade: None,
        }),
    }
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
    let signature_upgrade = admission.signature_upgrade;

    let lease = match state.acquire_library_work(library_id) {
        Ok(lease) => lease,
        Err(e) => {
            let store = state.store.lock();
            let report = with_signature_upgrade(
                with_error(
                    merge_scan_reports(&base_report, &serde_json::json!({})),
                    &e,
                    None,
                ),
                signature_upgrade.as_deref(),
            );
            finalize_scan(&store, op_id, expected_dirty_rev, ScanOperationStatus::Failed, &report);
            return;
        }
    };

    state.scan_coordinator.register_control(op_id, lease.control().clone());

    {
        let store = state.store.lock();
        // 只改状态：requeue 事务刚写入的跨轮累计报告必须保留（P3-NEW-1）。
        if let Err(error) = store.set_scan_operation_status(op_id, ScanOperationStatus::Running) {
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
            let report = with_signature_upgrade(
                merge_scan_reports(&base_report, &report_data),
                signature_upgrade.as_deref(),
            );
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
            // 失败终态：已提交计数 + 有界失败历史 + 错误摘要（P3-NEW-2）。
            let http_status = failure.response.status().as_u16();
            let report = with_signature_upgrade(
                with_error(
                    merge_scan_reports(&base_report, &failure.report),
                    &failure.error,
                    Some(http_status),
                ),
                signature_upgrade.as_deref(),
            );
            finalize_scan(&store, op_id, expected_dirty_rev, ScanOperationStatus::Failed, &report);
        }
        Err(e) => {
            let report = with_signature_upgrade(
                with_error(
                    merge_scan_reports(&base_report, &serde_json::json!({})),
                    &e.to_string(),
                    None,
                ),
                signature_upgrade.as_deref(),
            );
            finalize_scan(&store, op_id, expected_dirty_rev, ScanOperationStatus::Failed, &report);
        }
    }
}

/// 把签名升级原因写进终态报告（P2-NEW-1）：旧 operation 不得无声消失。
fn with_signature_upgrade(
    mut report: serde_json::Value,
    reason: Option<&str>,
) -> serde_json::Value {
    let Some(reason) = reason else {
        return report;
    };
    if !report.is_object() {
        report = serde_json::json!({});
    }
    report["signature_upgrade"] = serde_json::json!(reason);
    report
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

/// 累计报告里保留的失败条数上限（P3-NEW-2）：历史有界，`failure_count` 仍记总数。
const MAX_REPORT_FAILURES: usize = 5;

/// 跨轮累计：把本轮报告并入已持久化的累计报告（P3-3）。
///
/// 计数相加、roots 按路径合并、issues 截断到 20 条；status 由合并结果重算。
/// 失败历史（`failures` / `failure_count`）与签名升级原因跨轮保留：一次成功的补扫
/// 可以让 operation 以 `completed` 结束，但失败记录不能被抹掉（P3-NEW-2）。
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
    let failure_count = sum_report_field(base, round, "failure_count");
    let mut merged = serde_json::json!({
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
    });
    if failure_count > 0 {
        merged["failure_count"] = serde_json::json!(failure_count);
        let failures = merged_failures(base, round);
        if let Some(last) = failures.last() {
            merged["last_failure"] = last.clone();
        }
        merged["failures"] = serde_json::json!(failures);
    }
    if let Some(reason) = base
        .get("signature_upgrade")
        .or_else(|| round.get("signature_upgrade"))
    {
        merged["signature_upgrade"] = reason.clone();
    }
    merged
}

/// 合并两侧的失败历史并截断到 [`MAX_REPORT_FAILURES`] 条（保留最近发生的）。
fn merged_failures(base: &serde_json::Value, round: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut failures: Vec<serde_json::Value> = [base, round]
        .into_iter()
        .flat_map(|source| {
            source
                .get("failures")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .collect();
    if failures.len() > MAX_REPORT_FAILURES {
        let excess = failures.len() - MAX_REPORT_FAILURES;
        failures.drain(0..excess);
    }
    failures
}

/// 失败终态：保留累计计数，记录有界失败历史与错误摘要（P3-3 / P3-NEW-2）。
fn with_error(
    mut report: serde_json::Value,
    error: &str,
    http_status: Option<u16>,
) -> serde_json::Value {
    if !report.is_object() {
        report = serde_json::json!({});
    }
    report["status"] = serde_json::json!("failed");
    report["error"] = serde_json::json!(error);
    if let Some(status) = http_status {
        report["http_status"] = serde_json::json!(status);
    }
    let failure_count = report
        .get("failure_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        + 1;
    report["failure_count"] = serde_json::json!(failure_count);
    let mut failures = report
        .get("failures")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    failures.push(serde_json::json!({
        "error": error,
        "http_status": http_status,
        "status": "failed",
    }));
    if failures.len() > MAX_REPORT_FAILURES {
        let excess = failures.len() - MAX_REPORT_FAILURES;
        failures.drain(0..excess);
    }
    if let Some(last) = failures.last() {
        report["last_failure"] = last.clone();
    }
    report["failures"] = serde_json::json!(failures);
    report
}

#[cfg(test)]
mod tests;
