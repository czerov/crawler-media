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

pub fn prune_terminal_window(
    conn: &Connection,
    cutoff: i64,
    limit: usize,
    dry_run: bool,
) -> Result<RetentionWindow, JobError> {
    let limit = limit.clamp(1, 500);
    let tx = conn.unchecked_transaction()?;

    // Read current cursor
    let cursor_opt: Option<(i64, i64, i64)> = tx
        .query_row(
            "SELECT finished_at, last_rowid, cutoff FROM retention_cursors WHERE def_id = '__global__'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();

    let (cur_finished_at, cur_rowid) = match cursor_opt {
        Some((f, r, c)) if c == cutoff => (f, r),
        _ => (0, 0),
    };

    // Query terminal jobs ordered by (finished_at, rowid) strictly after current cursor
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

    struct RawRow {
        rowid: i64,
        id: String,
        def_id: Option<String>,
        finished_at: i64,
        run_after: i64,
    }

    let rows: Vec<RawRow> = {
        let rows_iter = stmt.query_map(params![cutoff, cur_finished_at, cur_rowid, limit + 1], |r| {
            Ok(RawRow {
                rowid: r.get(0)?,
                id: r.get(1)?,
                def_id: r.get(2)?,
                finished_at: r.get(3)?,
                run_after: r.get(4)?,
            })
        })?;
        rows_iter.collect::<Result<Vec<_>, _>>()?
    };
    drop(stmt);
    let window_has_more = rows.len() > limit;
    let window_rows: Vec<&RawRow> = rows.iter().take(limit).collect();
    let scanned = window_rows.len();

    let mut to_delete = Vec::new();
    let mut protected = 0;

    for row in &window_rows {
        if let Some(ref did) = row.def_id {
            // Check if this row is protected by latest terminal child rule
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
                // It is the latest terminal child, protect it!
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

    let cycle_complete = !window_has_more && window_rows.is_empty();

    if !dry_run {
        if let Some(last) = window_rows.last() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            tx.execute(
                "INSERT OR REPLACE INTO retention_cursors (def_id, cutoff, finished_at, last_rowid, cycle_started_at)
                 VALUES ('__global__', ?1, ?2, ?3, ?4)",
                params![cutoff, last.finished_at, last.rowid, now],
            )?;
        } else if cycle_complete {
            tx.execute("DELETE FROM retention_cursors WHERE def_id = '__global__'", [])?;
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
