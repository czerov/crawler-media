use rusqlite::{OptionalExtension, params};

use super::super::{ProbeJob, Store, StoreError};
use super::{now_ms, probe_job_terminal_status};

#[derive(Clone, Copy, Debug, Default)]
struct ProbeCounts {
    completed: usize,
    succeeded: usize,
    failed: usize,
    partial: usize,
}

impl ProbeCounts {
    fn for_status(status: &str) -> Self {
        let mut counts = Self { completed: 1, ..Self::default() };
        match status {
            "succeeded" => counts.succeeded = 1,
            "failed" => counts.failed = 1,
            "partial" => counts.partial = 1,
            "cancelled" => {}
            _ => counts.completed = 0,
        }
        counts
    }
}

impl Store {
    pub(super) fn finish_probe_transition(
        &self,
        job_id: &str,
        ledger_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(ProbeJob, bool, bool, i64), StoreError> {
        let started_at = std::time::Instant::now();
        let mut vm_steps = 0_i64;
        let tx = self.library.unchecked_transaction()?;
        let existing = {
            let mut statement = tx.prepare(
                "SELECT total, kind, completed, succeeded, failed, partial
                 FROM probe_jobs WHERE id = ?1 AND status IN ('queued', 'running')",
            )?;
            let result = statement.query_row([job_id], |row: &rusqlite::Row<'_>| Ok((
                row.get::<_, usize>(0)?, row.get::<_, String>(1)?, ProbeCounts {
                    completed: row.get::<_, usize>(2)?, succeeded: row.get::<_, usize>(3)?,
                    failed: row.get::<_, usize>(4)?, partial: row.get::<_, usize>(5)?,
                },
            ))).optional()?;
            vm_steps += i64::from(statement.get_status(rusqlite::StatementStatus::VmStep));
            result
        };
        let Some((total, kind, current)) = existing else {
            tx.commit()?;
            let job = self.get_probe_job(job_id)?.ok_or_else(|| StoreError::Missing(format!("probe job {job_id}")))?;
            let all_done = job.completed >= job.total;
            return Ok((job, false, all_done, vm_steps));
        };
        let changed = {
            let mut statement = tx.prepare(
                "UPDATE probe_job_units SET status = ?3, error = ?4
                 WHERE job_id = ?1 AND ledger_id = ?2 AND status IN ('queued', 'running')",
            )?;
            let changed = statement.execute(params![job_id, ledger_id, status, error])?;
            vm_steps += i64::from(statement.get_status(rusqlite::StatementStatus::VmStep));
            changed
        };
        let delta = if changed == 1 { ProbeCounts::for_status(status) } else { ProbeCounts::default() };
        let completed = current.completed + delta.completed;
        let succeeded = current.succeeded + delta.succeeded;
        let failed = current.failed + delta.failed;
        let partial = current.partial + delta.partial;
        let all_done = completed >= total;
        if changed == 1 {
            let terminal = probe_job_terminal_status(&kind, total, completed, failed, partial);
            let mut statement = tx.prepare(
                "UPDATE probe_jobs SET completed=?2, succeeded=?3, failed=?4, partial=?5,
                     status=COALESCE(?6, 'running'), finished_at_ms=COALESCE(?7, finished_at_ms),
                     error=COALESCE(?8, error)
                 WHERE id=?1 AND status IN ('queued','running')",
            )?;
            statement.execute(params![job_id, completed, succeeded, failed, partial, terminal,
                terminal.map(|_| now_ms()), error])?;
            vm_steps += i64::from(statement.get_status(rusqlite::StatementStatus::VmStep));
        }
        let accepted = changed == 1;
        if accepted {
            tracing::debug!(
                job_id, ledger_id, completed, succeeded, failed, partial,
                latency_us = started_at.elapsed().as_micros() as u64,
                "probe unit transition counters updated"
            );
        }
        tx.commit()?;
        let job = self.get_probe_job(job_id)?.ok_or_else(|| StoreError::Missing(format!("probe job {job_id}")))?;
        Ok((job, accepted, all_done, vm_steps))
    }

    pub(super) fn reconcile_job_counts(&self, job_id: &str) -> Result<ProbeJob, StoreError> {
        let tx = self.library.unchecked_transaction()?;
        reconcile_counts_in_transaction(&tx, job_id)?;
        tx.commit()?;
        self.get_probe_job(job_id)?.ok_or_else(|| StoreError::Missing(format!("probe job {job_id}")))
    }
}

pub(super) fn reconcile_counts_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    job_id: &str,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE probe_jobs SET
             completed=(SELECT COUNT(*) FROM probe_job_units WHERE job_id=?1 AND status IN ('succeeded','failed','cancelled','partial')),
             succeeded=(SELECT COUNT(*) FROM probe_job_units WHERE job_id=?1 AND status='succeeded'),
             failed=(SELECT COUNT(*) FROM probe_job_units WHERE job_id=?1 AND status='failed'),
             partial=(SELECT COUNT(*) FROM probe_job_units WHERE job_id=?1 AND status='partial')
         WHERE id=?1",
        [job_id],
    )?;
    Ok(())
}
