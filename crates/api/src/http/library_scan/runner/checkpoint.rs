//! `scan_seen` 断点判定与变化文件的派生事实失效（P2-3 / P2-NEW-2）。
//!
//! 文件版本事实（size/mtime，`.strm` 另有受限内容指纹）必须在 Store 锁外采集；进锁后
//! 只做一次批量 DB 读与纯内存判定。

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
    let checkpoints = {
        let store = state.store.lock();
        store.scan_seen_checkpoints(op_id, &paths)
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
            None => fresh.push(file),
        }
    }
    CheckpointDecision {
        fresh,
        skipped,
        changed,
        stamps,
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
