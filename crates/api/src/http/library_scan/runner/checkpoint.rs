//! `scan_seen` 断点判定与变化文件的派生事实失效（P2-3 / P2-NEW-2）。
//!
//! 文件版本事实（size/mtime，`.strm` 另有受限内容指纹）必须在 Store 锁外采集；进锁后
//! 只做一次批量 DB 读与纯内存判定。
//!
//! 变化检测有两个来源，缺一不可：当前 operation 的 `scan_seen` 断点（本轮已处理过的
//! 文件），以及 ledger 行上的 **durable 源版本**（上一个 operation 处理时的版本）。
//! 后者是跨 operation 检测的唯一依据——operation 终态会清空 `scan_seen`（P2-3）。

use std::collections::HashMap;
use std::path::PathBuf;

use crate::management::ApiState;

#[cfg(test)]
use super::hooks::test_observe_hook;

/// 断点判定结果（P2-NEW-2 / P2-3）。
pub(super) struct CheckpointDecision {
    pub(super) fresh: Vec<library::TransferredFile>,
    pub(super) skipped: usize,
    /// 命中断点行但版本已变化（或台账失效）的路径：必须失效派生事实并重排探测。
    pub(super) changed: Vec<PathBuf>,
    /// 锁外采集到的版本事实：`path -> (mtime_ms, file_size, content_digest)`。
    pub(super) stamps: HashMap<String, (i64, i64, Option<String>)>,
}

/// 断点续扫：丢掉本 operation 已经处理过、且仍然有效的文件（P2-NEW-2 / P2-3）。
///
/// 读检查点失败时按「未处理」继续——宁可重复一遍，也不能漏文件。
pub(super) fn checkpoint_fresh(
    state: &ApiState,
    op_id: Option<&str>,
    files: Vec<library::TransferredFile>,
) -> CheckpointDecision {
    let Some(op_id) = op_id else {
        // 无 operation 归属 = 无断点，不需要采集版本事实。
        return CheckpointDecision {
            fresh: files,
            skipped: 0,
            changed: Vec::new(),
            stamps: HashMap::new(),
        };
    };
    let mut stamps = HashMap::with_capacity(files.len());
    for file in &files {
        // 测试屏障：位于文件版本观察窗口内（锁外），用来证明慢观察不持 Store 锁。
        #[cfg(test)]
        test_observe_hook::run(op_id);
        stamps.insert(
            file.path.display().to_string(),
            store::Store::scan_seen_stamp(&file.path),
        );
    }
    let paths: Vec<String> = files.iter().map(|f| f.path.display().to_string()).collect();
    let (checkpoints, source_versions) = {
        let store = state.store.lock();
        // 两个读都在同一把 Store 锁内：断点与 durable 版本必须是同一时刻的一致视图。
        (
            store.scan_seen_checkpoints(op_id, &paths),
            store.ledger_source_versions(&paths),
        )
    };
    let checkpoints = match checkpoints {
        Ok(checkpoints) => checkpoints,
        Err(error) => {
            tracing::error!(%error, %op_id, "读取扫描断点失败，按未处理继续");
            return CheckpointDecision {
                fresh: files,
                skipped: 0,
                changed: Vec::new(),
                stamps,
            };
        }
    };
    let source_versions = source_versions.unwrap_or_else(|error| {
        // 读不到 durable 版本时按「没有版本事实」继续：与 `None` 分支同一保守方向
        // （有台账行但版本未知 → 视为变化），不会静默跳过变化文件。
        tracing::error!(%error, %op_id, "读取台账 durable 源版本失败，按版本未知处理");
        HashMap::new()
    });
    let mut fresh = Vec::with_capacity(files.len());
    let mut changed = Vec::new();
    let mut skipped = 0usize;
    for file in files {
        let key = file.path.display().to_string();
        let current = stamps.get(&key).cloned().unwrap_or((0, 0, None));
        match checkpoints.get(&key) {
            Some((mtime_ms, file_size, digest, true))
                if store::Store::scan_seen_stamp_matches(
                    &(*mtime_ms, *file_size, digest.clone()),
                    &current,
                ) =>
            {
                skipped += 1;
            }
            Some(_) => {
                changed.push(file.path.clone());
                fresh.push(file);
            }
            None => {
                // P2-3：本 operation 对该路径没有断点（首次扫到，或上一个 operation 已
                // 清空 `scan_seen`）。此时只能靠台账上的 durable 源版本判断文件自上次
                // 处理以来是否变化。
                if source_version_changed(source_versions.get(&key), &current) {
                    changed.push(file.path.clone());
                }
                fresh.push(file);
            }
        }
    }
    CheckpointDecision {
        fresh,
        skipped,
        changed,
        stamps,
    }
}

/// durable 源版本是否证明该台账行对应的文件已经变化（P2-3）。
///
/// - 没有版本记录 = 根本没有台账行：本轮新文件，登记即可，没有派生事实需要失效。
/// - 有台账行但版本未知（`0/0` 且无内容摘要，例如本特性上线前登记的旧行）：绝不视为
///   同一版本——宁可多探测一次，也不能静默沿用已经过期的派生事实。
fn source_version_changed(
    recorded: Option<&(i64, i64, Option<String>)>,
    current: &(i64, i64, Option<String>),
) -> bool {
    match recorded {
        None => false,
        Some(recorded) => !store::Store::scan_seen_stamp_matches(recorded, current),
    }
}

/// 变化文件：失效派生探测事实，并重挂 durable probe 义务（P2-3）。
///
/// 返回实际完成失效的路径，供调用方立即请求探测（obligation 是崩溃后的兜底）。
pub(super) fn invalidate_changed_facts(state: &ApiState, changed: &[PathBuf]) -> Vec<PathBuf> {
    if changed.is_empty() {
        return Vec::new();
    }
    let now_ms = crate::job_loop::unix_now() * 1000;
    let store = state.store.lock();
    let mut affected = Vec::new();
    for path in changed {
        let path_str = path.display().to_string();
        let Some(row) = store.ledger_by_path(&path_str).ok().flatten() else {
            continue;
        };
        let ledger_id = row.id.to_string();
        if let Err(error) = store.delete_file_meta_by_ledger_id(&ledger_id) {
            tracing::error!(%error, %ledger_id, %path_str, "变化文件的派生探测事实失效失败");
            continue;
        }
        if let Err(error) = store.arm_scan_probe_obligation(&ledger_id, now_ms, "scan_changed") {
            tracing::error!(%error, %ledger_id, "变化文件重挂探测义务失败");
            continue;
        }
        affected.push(path.clone());
    }
    affected
}
