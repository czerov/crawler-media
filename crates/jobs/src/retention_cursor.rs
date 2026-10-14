//! Keyset window retention with durable cursors.

use rusqlite::{Connection, params};

use crate::types::JobError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionWindow {
    pub scanned: usize,
    pub candidates: usize,
    pub protected: usize,
    pub deleted: usize,
    pub window_has_more: bool,
    pub cycle_complete: bool,
}

pub fn migrate_retention_cursors(conn: &Connection) -> Result<(), JobError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS retention_cursors (
             def_id TEXT PRIMARY KEY,
             cutoff INTEGER NOT NULL,
             finished_at INTEGER NOT NULL,
             last_rowid INTEGER NOT NULL,
             cycle_started_at INTEGER NOT NULL
         );",
    )?;
    Ok(())
}

struct RawRow {
    rowid: i64,
    id: String,
    def_id: Option<String>,
    finished_at: i64,
    run_after: i64,
}

fn fetch_candidate_rows(
    tx: &rusqlite::Transaction<'_>,
    cutoff: i64,
    cur_finished_at: i64,
    cur_rowid: i64,
    limit: usize,
) -> Result<Vec<RawRow>, JobError> {
    let mut stmt = tx.prepare(
        "SELECT rowid, id, def_id, finished_at, run_after
         FROM jobs
         WHERE status IN ('succeeded', 'failed', 'cancelled')
           AND finished_at IS NOT NULL
           AND finished_at < ?1
           AND (finished_at > ?2 OR (finished_at = ?2 AND rowid > ?3))
         ORDER BY finished_at ASC, rowid ASC
         LIMIT ?4",
    )?;
    let rows_iter = stmt.query_map(params![cutoff, cur_finished_at, cur_rowid, limit + 1], |r| {
        Ok(RawRow {
            rowid: r.get(0)?,
            id: r.get(1)?,
            def_id: r.get(2)?,
            finished_at: r.get(3)?,
            run_after: r.get(4)?,
        })
    })?;
    rows_iter.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn process_window_deletion(
    tx: &rusqlite::Transaction<'_>,
    window_rows: &[&RawRow],
    dry_run: bool,
) -> Result<(usize, usize, usize), JobError> {
    let mut to_delete = Vec::new();
    let mut protected = 0;

    for row in window_rows {
        if let Some(ref did) = row.def_id {
            let is_latest: bool = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM jobs
                     WHERE def_id = ?1
                       AND status IN ('succeeded', 'failed', 'cancelled')
                       AND (finished_at > ?2 OR (finished_at = ?2 AND run_after > ?3))
                 )",
                params![did, row.finished_at, row.run_after],
                |r| r.get(0),
            )?;
            if !is_latest {
                protected += 1;
                continue;
            }
        }
        to_delete.push(&row.id);
    }

    let candidates = to_delete.len();
    let deleted = if dry_run {
        0
    } else {
        for chunk in to_delete.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let sql = format!("DELETE FROM jobs WHERE id IN ({placeholders})");
            tx.execute(&sql, rusqlite::params_from_iter(chunk))?;
        }
        candidates
    };

    Ok((candidates, protected, deleted))
}

/// 单个 keyset 窗口。
///
/// `cursor_key` 是 `retention_cursors.def_id`：按 job def 隔离游标（B3 §5.1
/// "每 job_def 在 jobs.db 保存 retention_cursors"），避免两个 retention def
/// 共用一条游标互相推进。
pub fn prune_terminal_window(
    conn: &Connection,
    cursor_key: &str,
    cutoff: i64,
    limit: usize,
    dry_run: bool,
) -> Result<RetentionWindow, JobError> {
    let limit = limit.clamp(1, 500);
    let tx = conn.unchecked_transaction()?;

    let cursor_opt: Option<(i64, i64, i64)> = match tx.query_row(
        "SELECT finished_at, last_rowid, cutoff FROM retention_cursors WHERE def_id = ?1",
        params![cursor_key],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ) {
        Ok(cursor) => Some(cursor),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(error) => {
            tracing::warn!(%error, cursor_key, "读取 retention 游标失败，本轮按新周期重新开始");
            None
        }
    };

    // Fixed cycle cutoff: continue cycle if current cutoff is at least stored cutoff
    let (cur_finished_at, cur_rowid, active_cutoff) = match cursor_opt {
        Some((f, r, c)) if cutoff >= c => (f, r, c),
        _ => (0, 0, cutoff),
    };

    let rows = fetch_candidate_rows(&tx, active_cutoff, cur_finished_at, cur_rowid, limit)?;
    let window_has_more = rows.len() > limit;
    let window_rows: Vec<&RawRow> = rows.iter().take(limit).collect();
    let scanned = window_rows.len();

    let (candidates, protected, deleted) = process_window_deletion(&tx, &window_rows, dry_run)?;
    // 窗口未满即说明这个 keyset 范围内已经没有更多候选：cycle 到此结束，清游标。
    // 旧实现只在**下一个空窗口**才结束 cycle，每个 cycle 都会多空跑一次。
    let cycle_complete = !window_has_more;

    if !dry_run {
        if cycle_complete {
            // cycle 结束：清游标，下一轮从新 cutoff 从头检查新超龄/迟到插入，
            // 不需要再空跑一个空窗口来收尾。
            tx.execute(
                "DELETE FROM retention_cursors WHERE def_id = ?1",
                params![cursor_key],
            )?;
        } else if let Some(last) = window_rows.last() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            tx.execute(
                "INSERT OR REPLACE INTO retention_cursors (def_id, cutoff, finished_at, last_rowid, cycle_started_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![cursor_key, active_cutoff, last.finished_at, last.rowid, now],
            )?;
        }
    }

    tx.commit()?;

    Ok(RetentionWindow {
        scanned,
        candidates,
        protected,
        deleted,
        window_has_more,
        cycle_complete,
    })
}
