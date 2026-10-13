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
//! 统计口径：候选窗口**只含可删行**（被 last-child 保护的行不进窗口，否则最旧一批
//! 全是受保护行时清理会永久失效并报成功——DB8 复核 C1）。因此 `scanned` 与
//! `candidates` 都是本批可删行数（≤ `batch_size`），`protected` 是同时间范围内被
//! last-child 规则保留的行数（报告用诊断，不参与删除），`deleted` 是真正删除的行数
//! （`dry_run` 恒为 0）。`has_more` 表示窗口之外仍有**可删**候选。

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
    // 窗口已只含可删行；`protected` 只作为报告诊断（同时间范围内被保护的行数）。
    let protected = protected_window(&tx, cutoff, batch_size)?;
    let deleted = if dry_run {
        0
    } else {
        delete_ids(&tx, &candidates)?
    };
    tx.commit()?;

    let batch = RetentionBatch {
        scanned: candidates.len(),
        candidates: candidates.len(),
        deleted,
        protected,
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

/// 「可删」谓词：`def_id IS NULL`，或同 def 存在**严格更新**的终态兄弟（即本行不在
/// `get_last_child` 会读到的并列组里）。
///
/// 它必须出现在**候选窗口**里，而不是只用于事后过滤：否则当最旧的一批恰好全是各自
/// def 的最新终态行时，窗口永远只含被保护行 → `deleted` 恒为 0 → 调用方按"没有可删
/// 候选"停止，清理永久失效却报告成功（DB8 复核 C1）。
fn deletable_predicate() -> String {
    format!(
        "def_id IS NULL OR EXISTS (
        SELECT 1 FROM jobs newer
        WHERE newer.def_id = jobs.def_id
          AND newer.status IN ({TERMINAL})
          AND newer.finished_at IS NOT NULL
          AND (newer.finished_at > jobs.finished_at
               OR (newer.finished_at = jobs.finished_at AND newer.run_after > jobs.run_after))
    )"
    )
}

/// 候选窗口：超龄**且可删**的终态行从旧到新，取至多 `batch_size + 1` 行，多出的一行
/// 只用于判断 `has_more`，不进入本批删除范围。
///
/// `ORDER BY finished_at, rowid` 与 `jobs_finished_idx` 的自然顺序一致，不需要临时排序，
/// 也不会因为并列 `finished_at` 而漏行或重复行。
fn candidate_window(
    conn: &Connection,
    cutoff: i64,
    batch_size: usize,
) -> Result<Vec<String>, JobError> {
    let deletable = deletable_predicate();
    let mut stmt = conn.prepare(&format!(
        "SELECT id FROM jobs
         WHERE status IN ({TERMINAL}) AND finished_at IS NOT NULL AND finished_at < ?1
           AND ({deletable})
         ORDER BY finished_at ASC, rowid ASC
         LIMIT ?2"
    ))?;
    let limit = as_i64(batch_size.saturating_add(1));
    let rows = stmt.query_map(params![cutoff, limit], |row| row.get::<_, String>(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// 报告用诊断：同一时间范围内**被 last-child 保护**的行数。
///
/// 窗口已排除这些行，所以它们不再影响 `deleted`/`has_more`；这里只是让报告仍能显示
/// "还有多少行是因为保护而留着"。
fn protected_window(
    conn: &Connection,
    cutoff: i64,
    batch_size: usize,
) -> Result<usize, JobError> {
    let deletable = deletable_predicate();
    let limit = as_i64(batch_size.saturating_add(1));
    let count: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM (
                 SELECT id FROM jobs
                 WHERE status IN ({TERMINAL}) AND finished_at IS NOT NULL AND finished_at < ?1
                   AND NOT ({deletable})
                 ORDER BY finished_at ASC, rowid ASC
                 LIMIT ?2
             )"
        ),
        params![cutoff, limit],
        |row| row.get(0),
    )?;
    Ok(usize::try_from(count.max(0)).unwrap_or(usize::MAX))
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
