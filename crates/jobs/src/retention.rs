//! 终止态 Jobs 历史的按龄分批保留（DB8 Task 2）。
//!
//! 规则见 `docs/superpowers/plans/2026-10-12-db8-maintenance-retention.md` 第 3.3 / 5.1 节：
//!
//! - 只有 `succeeded / failed / cancelled` 且 `finished_at IS NOT NULL AND finished_at < cutoff`
//!   才是候选；`finished_at` 为 NULL 或等于 `cutoff` 一律保留。
//! - queued / running（含正在重试的 queued）永不删除。
//! - 每个 `def_id` 保留 `get_last_child` 会读到的那一条（终态中
//!   `finished_at DESC, run_after DESC` 的首行）；若 `finished_at` 与 `run_after` 完全并列，
//!   整个并列组保留。`def_id IS NULL` 的独立历史不分组保护，按年龄删除。
//! - 不删除 `job_defs`；单次最多检视/处理 `batch_size` 行；每批一个事务；`dry_run` 零写入。
//!
//! 统计口径（与 Task 3 的 `PlaybackRetentionBatch` 同义）：`scanned` 与 `candidates` 都是本批
//! 检视的超龄终态行数（≤ `batch_size`，未超龄的行不进入窗口），`protected` 是其中被
//! last-child 规则保留的行数，`deleted` 是真正删除的行数（`dry_run` 恒为 0）。
//! `has_more` 表示窗口之外仍有超龄候选；被保护的候选同样算候选，因此调用方必须像维护
//! runner 一样自行限制批数，不能仅凭 `has_more` 无限循环。

use std::collections::HashSet;

use rusqlite::{Connection, params, params_from_iter};

use crate::types::JobError;

/// 单批终止态清理的统计。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetentionBatch {
    pub scanned: usize,
    pub candidates: usize,
    pub deleted: usize,
    pub protected: usize,
    pub has_more: bool,
}

/// 终态状态集合，与 `schedule::get_last_child` 保持一致。
const TERMINAL: &str = "'succeeded','failed','cancelled'";
/// 单条 DELETE 的最大绑定参数，避免触碰 SQLITE_MAX_VARIABLE_NUMBER。
const DELETE_CHUNK: usize = 400;

/// 一批：事务内选择候选、判定保护、删除；批间由调用方释放（本函数一次只处理一批）。
pub(crate) fn prune_terminal_jobs(
    conn: &Connection,
    cutoff: i64,
    batch_size: usize,
    dry_run: bool,
) -> Result<RetentionBatch, JobError> {
    if batch_size == 0 {
        return Ok(RetentionBatch::default());
    }
    let tx = conn.unchecked_transaction()?;
    let window = candidate_window(&tx, cutoff, batch_size)?;
    let has_more = window.len() > batch_size;
    let candidates: Vec<&str> = window.iter().take(batch_size).map(String::as_str).collect();
    let protected = protected_ids(&tx, &candidates)?;
    let deletable: Vec<&str> = candidates
        .iter()
        .copied()
        .filter(|id| !protected.contains(*id))
        .collect();
    let deleted = if dry_run {
        0
    } else {
        delete_ids(&tx, &deletable)?
    };
    tx.commit()?;

    let batch = RetentionBatch {
        scanned: candidates.len(),
        candidates: candidates.len(),
        deleted,
        protected: protected.len(),
        has_more,
    };
    tracing::info!(
        cutoff,
        batch_size,
        dry_run,
        scanned = batch.scanned,
        candidates = batch.candidates,
        protected = batch.protected,
        deleted = batch.deleted,
        has_more = batch.has_more,
        "终止态任务历史清理批次完成"
    );
    Ok(batch)
}

/// 候选窗口：超龄终态行从旧到新，取至多 `batch_size + 1` 行，多出的一行只用于判断
/// `has_more`，不进入本批删除范围。
///
/// `ORDER BY finished_at, rowid` 与 `jobs_finished_idx` 的自然顺序一致，不需要临时排序，
/// 也不会因为并列 `finished_at` 而漏行或重复行。
fn candidate_window(
    conn: &Connection,
    cutoff: i64,
    batch_size: usize,
) -> Result<Vec<String>, JobError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id FROM jobs
         WHERE status IN ({TERMINAL}) AND finished_at IS NOT NULL AND finished_at < ?1
         ORDER BY finished_at ASC, rowid ASC
         LIMIT ?2"
    ))?;
    let limit = as_i64(batch_size.saturating_add(1));
    let rows = stmt.query_map(params![cutoff, limit], |row| row.get::<_, String>(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// 候选里属于各自 def 最新并列组的 id —— 即 `get_last_child` 可能读到的那一组。
///
/// `def_id IS NULL` 不在保护范围内；def 是否还在 `job_defs` 中与保护无关。
fn protected_ids(conn: &Connection, candidates: &[&str]) -> Result<HashSet<String>, JobError> {
    if candidates.is_empty() {
        return Ok(HashSet::new());
    }
    let placeholders = vec!["?"; candidates.len()].join(",");
    let sql = format!(
        "SELECT c.id FROM jobs c
         WHERE c.id IN ({placeholders})
           AND c.def_id IS NOT NULL
           AND NOT EXISTS (
               SELECT 1 FROM jobs l
               WHERE l.def_id = c.def_id
                 AND l.status IN ({TERMINAL})
                 AND l.finished_at IS NOT NULL
                 AND (l.finished_at > c.finished_at
                      OR (l.finished_at = c.finished_at AND l.run_after > c.run_after))
           )"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(candidates.iter()), |row| {
        row.get::<_, String>(0)
    })?;
    rows.collect::<Result<HashSet<_>, _>>().map_err(Into::into)
}

fn delete_ids(conn: &Connection, ids: &[&str]) -> Result<usize, JobError> {
    let mut deleted = 0;
    for chunk in ids.chunks(DELETE_CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!("DELETE FROM jobs WHERE id IN ({placeholders})");
        deleted += conn.execute(&sql, params_from_iter(chunk.iter()))?;
    }
    Ok(deleted)
}

fn as_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
