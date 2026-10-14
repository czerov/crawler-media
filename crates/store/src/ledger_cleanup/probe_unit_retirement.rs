//! Probe work items that outlive their ledger row.
//!
//! `probe_job_units` is the *live* work queue while `probe_jobs.total/completed/
//! succeeded/failed` are derived from it. A unit whose ledger row is gone can
//! never run again, so it is dropped — and the job is resynced instead of being
//! left `running` with a completion predicate it can no longer satisfy.
//!
//! The same derivation is enforced whenever a ledger row disappears, and once
//! from the library schema migration for databases written before that.

use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;

/// Remove every probe work item for `ledger_id` and resync the jobs that owned them.
pub(crate) fn retire_probe_units_for_ledger(
    conn: &Connection,
    ledger_id: &str,
) -> Result<(), StoreError> {
    let job_ids: Vec<String> = conn
        .prepare("SELECT DISTINCT job_id FROM probe_job_units WHERE ledger_id = ?1")?
        .query_map([ledger_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if job_ids.is_empty() {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM probe_job_units WHERE ledger_id = ?1",
        [ledger_id],
    )?;
    for job_id in &job_ids {
        resync_probe_job(conn, job_id)?;
    }
    Ok(())
}

/// Recompute `probe_jobs` counters from the live units, and terminalize an
/// active job whose remaining work is all terminal instead of leaving it
/// `running` with a completion predicate nothing can satisfy.
///
/// A job with no units left at all is cancelled (`file_deleted`): its whole
/// scope disappeared. A job that still has a genuinely outstanding unit is
/// untouched; one whose remaining units all finished takes the same per-kind
/// terminal state as the normal completion path.
pub(crate) fn resync_probe_job(conn: &Connection, job_id: &str) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE probe_jobs SET
             total = (SELECT COUNT(*) FROM probe_job_units WHERE job_id = ?1),
             completed = (SELECT COUNT(*) FROM probe_job_units WHERE job_id = ?1
                          AND status IN ('succeeded', 'failed', 'cancelled', 'partial')),
             succeeded = (SELECT COUNT(*) FROM probe_job_units
                          WHERE job_id = ?1 AND status = 'succeeded'),
             failed = (SELECT COUNT(*) FROM probe_job_units
                       WHERE job_id = ?1 AND status = 'failed')
         WHERE id = ?1",
        [job_id],
    )?;
    let job: Option<(String, String, i64, i64, i64, i64)> = conn
        .query_row(
            "SELECT kind, status, total, completed, failed,
                    (SELECT COUNT(*) FROM probe_job_units
                     WHERE job_id = probe_jobs.id AND status = 'partial')
             FROM probe_jobs WHERE id = ?1",
            [job_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((kind, status, total, completed, failed, partial)) = job else {
        return Ok(());
    };
    if status != "queued" && status != "running" {
        return Ok(());
    }
    if total == 0 {
        conn.execute(
            "UPDATE probe_jobs SET status = 'cancelled',
                 error = COALESCE(error, 'file_deleted'),
                 finished_at_ms = COALESCE(finished_at_ms, ?2)
             WHERE id = ?1 AND status IN ('queued', 'running')",
            params![job_id, crate::unix_now() * 1000],
        )?;
        return Ok(());
    }
    let Some(terminal) = crate::probe_tasks::probe_job_terminal_status(
        &kind,
        total as usize,
        completed as usize,
        failed as usize,
        partial as usize,
    ) else {
        return Ok(());
    };
    conn.execute(
        "UPDATE probe_jobs SET status = ?2,
             finished_at_ms = COALESCE(finished_at_ms, ?3),
             error = COALESCE((SELECT error FROM probe_job_units
                               WHERE job_id = ?1 AND error IS NOT NULL LIMIT 1), error)
         WHERE id = ?1 AND status IN ('queued', 'running')",
        params![job_id, terminal, crate::unix_now() * 1000],
    )?;
    Ok(())
}

/// One-shot repair for databases written before ledger deletion retired probe
/// units: drop work items whose ledger row is gone and resync their jobs.
///
/// Bounded by the rows already present; runs from the library schema migration.
pub(crate) fn repair_orphan_probe_units(conn: &Connection) -> Result<(), StoreError> {
    let job_ids: Vec<String> = conn
        .prepare(
            "SELECT DISTINCT job_id FROM probe_job_units u
             WHERE NOT EXISTS (SELECT 1 FROM ledger l WHERE l.id = u.ledger_id)",
        )?
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if job_ids.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    let removed = tx.execute(
        "DELETE FROM probe_job_units
         WHERE NOT EXISTS (SELECT 1 FROM ledger l WHERE l.id = probe_job_units.ledger_id)",
        [],
    )?;
    for job_id in &job_ids {
        resync_probe_job(&tx, job_id)?;
    }
    tx.commit()?;
    tracing::info!(
        removed_units = removed,
        affected_jobs = job_ids.len(),
        "已清理指向不存在台账行的探测工作项"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
