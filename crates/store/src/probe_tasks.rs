mod counters;

use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError};

#[derive(Clone, Debug)]
pub struct ProbeJob {
    pub id: String,
    pub kind: String,
    pub media_id: String,
    pub season: Option<u32>,
    pub status: String,
    pub total: usize,
    pub completed: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub partial: usize,
    pub error: Option<String>,
    pub created_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct ProbeJobUnit {
    pub job_id: String,
    pub ledger_id: String,
    pub kind: String,
    pub force_fingerprint: bool,
    pub reuse_fingerprint_cache: bool,
    pub overwrite_markers: bool,
    pub status: String,
    pub error: Option<String>,
    pub sampling_plan_json: Option<String>,
    pub detection_outcome_json: Option<String>,
    pub reuse_media_info_cache: bool,
    /// Bounded dispatch window this unit belongs to; persisted, never guessed.
    pub stage: super::ProbeDispatchStage,
}

pub struct ProbeJobUnitSpec<'a> {
    pub ledger_id: &'a str,
    pub kind: &'a str,
    pub force_fingerprint: bool,
    pub reuse_fingerprint_cache: bool,
    pub overwrite_markers: bool,
    pub reuse_media_info_cache: bool,
}

impl ProbeJob {
    pub fn is_active(&self) -> bool {
        self.status == "queued" || self.status == "running"
    }

    pub fn elapsed_ms(&self, now_ms: i64) -> u64 {
        let end = self.finished_at_ms.unwrap_or(now_ms);
        let start = self.started_at_ms.unwrap_or(self.created_at_ms);
        end.saturating_sub(start).max(0) as u64
    }
}

impl Store {
    pub fn create_probe_job(
        &self,
        id: &str,
        kind: &str,
        media_id: &str,
        season: Option<u32>,
        scope_key: &str,
        units: &[ProbeJobUnitSpec<'_>],
    ) -> Result<bool, StoreError> {
        if units.is_empty() {
            return Ok(false);
        }
        let tx = self.library.unchecked_transaction()?;
        let inserted = tx.execute(
            "INSERT INTO probe_jobs (
                 id, kind, media_id, season, scope_key, status, total, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, ?7)",
            params![
                id,
                kind,
                media_id,
                season.map(i64::from),
                scope_key,
                units.len(),
                now_ms()
            ],
        );
        if let Err(error) = inserted {
            if is_constraint(&error) {
                return Ok(false);
            }
            return Err(error.into());
        }
        for unit in units {
            if let Err(error) = tx.execute(
                "INSERT INTO probe_job_units (
                     job_id, ledger_id, kind, force_fingerprint,
                     reuse_fingerprint_cache, overwrite_markers, status,
                     reuse_media_info_cache, stage
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7, ?8)",
                params![
                    id,
                    unit.ledger_id,
                    unit.kind,
                    unit.force_fingerprint,
                    unit.reuse_fingerprint_cache,
                    unit.overwrite_markers,
                    unit.reuse_media_info_cache,
                    super::ProbeDispatchStage::for_job_kind(kind).as_str(),
                ],
            ) {
                if is_constraint(&error) {
                    return Ok(false);
                }
                return Err(error.into());
            }
        }
        tx.commit()?;
        Ok(true)
    }
    /// Requeue interrupted units and reconcile counters without loading work.
    ///
    /// Startup must not materialize every pending unit: the bounded dispatch
    /// windows decide what runs next, and the SQL here only repairs durable
    /// status/counters for active jobs.
    pub fn reset_running_probe_units(&self) -> Result<(), StoreError> {
        let tx = self.library.unchecked_transaction()?;
        tx.execute(
            "UPDATE probe_job_units SET status = 'queued'
             WHERE status = 'running' AND job_id IN (
                 SELECT id FROM probe_jobs WHERE status IN ('queued', 'running')
             )",
            [],
        )?;
        tx.execute(
            "UPDATE probe_jobs SET status = 'queued'
             WHERE status = 'running' AND EXISTS (
                 SELECT 1 FROM probe_job_units
                 WHERE job_id = probe_jobs.id AND status = 'queued'
             )",
            [],
        )?;
        tx.execute(
            "UPDATE probe_jobs SET
                 completed=(SELECT COUNT(*) FROM probe_job_units WHERE job_id=probe_jobs.id
                     AND status IN ('succeeded','failed','cancelled','partial')),
                 succeeded=(SELECT COUNT(*) FROM probe_job_units WHERE job_id=probe_jobs.id
                     AND status='succeeded'),
                 failed=(SELECT COUNT(*) FROM probe_job_units WHERE job_id=probe_jobs.id
                     AND status='failed'),
                 partial=(SELECT COUNT(*) FROM probe_job_units WHERE job_id=probe_jobs.id
                     AND status='partial')
             WHERE status IN ('queued','running')",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Active jobs of one kind with their units. Marker refreshes are rare
    /// enough to inspect whole; this is not used for ordinary backlog jobs.
    pub fn active_probe_jobs_by_kind(
        &self,
        kind: &str,
    ) -> Result<Vec<(ProbeJob, Vec<ProbeJobUnit>)>, StoreError> {
        let mut statement = self.library.prepare(
            "SELECT id, kind, media_id, season, status, total, completed, succeeded,
                    failed, partial, error, created_at_ms, started_at_ms, finished_at_ms
             FROM probe_jobs WHERE status IN ('queued', 'running') AND kind = ?1
             ORDER BY created_at_ms, id",
        )?;
        let jobs = statement
            .query_map([kind], map_probe_job)?
            .collect::<Result<Vec<_>, _>>()?;
        jobs.into_iter()
            .map(|job| {
                let units = self.probe_job_units(&job.id)?;
                Ok((job, units))
            })
            .collect()
    }

    /// Active non-marker jobs whose units are all terminal.
    pub fn active_completed_probe_jobs(
        &self,
        limit: usize,
    ) -> Result<Vec<ProbeJob>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut statement = self.library.prepare(
            "SELECT id, kind, media_id, season, status, total, completed, succeeded,
                    failed, partial, error, created_at_ms, started_at_ms, finished_at_ms
             FROM probe_jobs
             WHERE status IN ('queued', 'running') AND completed >= total
               AND kind != 'marker_refresh'
             ORDER BY created_at_ms, id LIMIT ?1",
        )?;
        let jobs = statement
            .query_map([limit as i64], map_probe_job)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(jobs)
    }

    /// A bounded window of active unit references for removed-library repair.
    pub fn active_probe_unit_refs(
        &self,
        limit: usize,
    ) -> Result<Vec<(String, String)>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut statement = self.library.prepare(
            "SELECT u.job_id, u.ledger_id
             FROM probe_job_units u
             JOIN probe_jobs j ON j.id = u.job_id
             WHERE u.status = 'queued' AND j.status IN ('queued', 'running')
             ORDER BY u.rowid LIMIT ?1",
        )?;
        let rows = statement.query_map([limit as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn recover_probe_jobs(&self) -> Result<Vec<(ProbeJob, Vec<ProbeJobUnit>)>, StoreError> {
        // Reconcile the counters of every active job once (D2 contract), but
        // never requeue a running unit: drain and retry assessment must observe
        // the job the live worker is finishing, not reset it back to queued.
        {
            let tx = self.library.unchecked_transaction()?;
            let ids: Vec<String> = {
                let mut statement = tx.prepare(
                    "SELECT id FROM probe_jobs WHERE status IN ('queued', 'running')
                     ORDER BY created_at_ms, id",
                )?;
                statement
                    .query_map([], |row| row.get(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            for id in &ids {
                counters::reconcile_counts_in_transaction(&tx, id)?;
            }
            tx.commit()?;
        }
        let mut statement = self.library.prepare(
            "SELECT id, kind, media_id, season, status, total, completed, succeeded,
                    failed, partial, error, created_at_ms, started_at_ms, finished_at_ms
             FROM probe_jobs WHERE status IN ('queued', 'running')
             ORDER BY created_at_ms, id",
        )?;
        let jobs = statement.query_map([], map_probe_job)?.collect::<Result<Vec<_>, _>>()?;
        jobs.into_iter().map(|job| {
            let units = self.probe_job_units(&job.id)?;
            Ok((job, units))
        }).collect()
    }


    pub fn probe_job_units(&self, job_id: &str) -> Result<Vec<ProbeJobUnit>, StoreError> {
        let mut statement = self.library.prepare(
            "SELECT job_id, ledger_id, kind, force_fingerprint,
                    reuse_fingerprint_cache, overwrite_markers, status, error,
                    sampling_plan_json, detection_outcome_json, reuse_media_info_cache, stage
             FROM probe_job_units WHERE job_id = ?1 ORDER BY rowid",
        )?;
        let rows = statement.query_map([job_id], |row| {
            Ok(ProbeJobUnit {
                job_id: row.get(0)?,
                ledger_id: row.get(1)?,
                kind: row.get(2)?,
                force_fingerprint: row.get(3)?,
                reuse_fingerprint_cache: row.get(4)?,
                overwrite_markers: row.get(5)?,
                status: row.get(6)?,
                error: row.get(7)?,
                sampling_plan_json: row.get(8)?,
                detection_outcome_json: row.get(9)?,
                reuse_media_info_cache: row.get::<_, i64>(10).unwrap_or(0) != 0,
                stage: row
                    .get::<_, String>(11)
                    .ok()
                    .and_then(|value| super::ProbeDispatchStage::from_str(&value))
                    .unwrap_or(super::ProbeDispatchStage::Metadata),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn start_probe_unit(&self, job_id: &str, ledger_id: &str) -> Result<bool, StoreError> {
        let tx = self.library.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE probe_job_units SET status = 'running'
             WHERE job_id = ?1 AND ledger_id = ?2 AND status = 'queued'
             AND EXISTS (SELECT 1 FROM probe_jobs WHERE id = ?1 AND status IN ('queued', 'running'))",
            params![job_id, ledger_id],
        )?;
        if changed > 0 {
            tx.execute(
                "UPDATE probe_jobs SET status = 'running',
                     started_at_ms = COALESCE(started_at_ms, ?2)
                 WHERE id = ?1 AND status IN ('queued', 'running')",
                params![job_id, now_ms()],
            )?;
        }
        tx.commit()?;
        Ok(changed > 0)
    }

    pub fn put_probe_sampling_plan(
        &self,
        job_id: &str,
        ledger_id: &str,
        sampling_plan_json: &str,
    ) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "UPDATE probe_job_units SET sampling_plan_json = ?3
             WHERE job_id = ?1 AND ledger_id = ?2",
            params![job_id, ledger_id, sampling_plan_json],
        )?;
        Ok(changed > 0)
    }

    pub fn put_probe_detection_outcome(
        &self,
        job_id: &str,
        ledger_id: &str,
        detection_outcome_json: &str,
    ) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "UPDATE probe_job_units SET detection_outcome_json = ?3
             WHERE job_id = ?1 AND ledger_id = ?2",
            params![job_id, ledger_id, detection_outcome_json],
        )?;
        Ok(changed > 0)
    }

    pub fn finish_probe_unit(
        &self,
        job_id: &str,
        ledger_id: &str,
        succeeded: bool,
        error: Option<&str>,
    ) -> Result<(ProbeJob, bool, bool), StoreError> {
        let status = if succeeded { "succeeded" } else { "failed" };
        self.finish_probe_unit_with_status(job_id, ledger_id, status, None, error)
    }

    pub fn cancel_probe_unit(
        &self,
        job_id: &str,
        ledger_id: &str,
        reason: &str,
    ) -> Result<(ProbeJob, bool, bool), StoreError> {
        self.finish_probe_unit_with_status(job_id, ledger_id, "cancelled", Some("cancelled"), Some(reason))
    }

    /// Cancel an atomic refresh, terminalizing pending units and releasing the scope together.
    pub fn cancel_probe_job(&self, job_id: &str, reason: &str) -> Result<(), StoreError> {
        let tx = self.library.unchecked_transaction()?;
        tx.execute(
            "UPDATE probe_job_units SET status = 'cancelled', error = ?2
             WHERE job_id = ?1 AND status IN ('queued', 'running')
             AND EXISTS (SELECT 1 FROM probe_jobs WHERE id = ?1 AND status IN ('queued', 'running'))",
            params![job_id, reason],
        )?;
        tx.execute(
            "UPDATE probe_jobs SET status = 'cancelled', error = ?2, finished_at_ms = ?3
             WHERE id = ?1 AND status IN ('queued', 'running')",
            params![job_id, reason, now_ms()],
        )?;
        counters::reconcile_counts_in_transaction(&tx, job_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn finish_probe_unit_with_status(
        &self,
        job_id: &str,
        ledger_id: &str,
        status: &str,
        _error_kind: Option<&str>,
        error: Option<&str>,
    ) -> Result<(ProbeJob, bool, bool), StoreError> {
        let (job, changed, all_done, _) = self.finish_probe_transition(job_id, ledger_id, status, error)?;
        Ok((job, changed, all_done))
    }

    pub fn finish_probe_unit_with_status_and_vm_steps(
        &self,
        job_id: &str,
        ledger_id: &str,
        status: &str,
        error_kind: Option<&str>,
        error: Option<&str>,
    ) -> Result<(ProbeJob, bool, bool, i64), StoreError> {
        let _ = error_kind;
        self.finish_probe_transition(job_id, ledger_id, status, error)
    }

    pub fn reconcile_probe_job_counts(&self, job_id: &str) -> Result<ProbeJob, StoreError> {
        self.reconcile_job_counts(job_id)
    }

    pub fn finish_probe_job(
        &self,
        job_id: &str,
        succeeded: bool,
        error: Option<&str>,
    ) -> Result<(), StoreError> {
        self.library.execute(
            "UPDATE probe_jobs SET status = ?2, error = COALESCE(?3, error), finished_at_ms = ?4
             WHERE id = ?1 AND status IN ('queued', 'running')",
            params![
                job_id,
                if succeeded { "succeeded" } else { "failed" },
                error,
                now_ms()
            ],
        )?;
        Ok(())
    }

    /// Terminalize a fully completed job when startup cannot read its refreshed state.
    /// The completion predicate prevents this recovery fallback from failing pending work.
    pub fn fail_completed_probe_job(&self, job_id: &str, error: &str) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "UPDATE probe_jobs SET status = 'failed', error = COALESCE(error, ?2),
                    finished_at_ms = ?3
             WHERE id = ?1 AND status IN ('queued', 'running') AND completed = total",
            params![job_id, error, now_ms()],
        )?;
        Ok(changed > 0)
    }

    pub fn get_probe_job(&self, id: &str) -> Result<Option<ProbeJob>, StoreError> {
        self.library
            .query_row(
                "SELECT id, kind, media_id, season, status, total, completed, succeeded,
                        failed, partial, error, created_at_ms, started_at_ms, finished_at_ms
                 FROM probe_jobs WHERE id = ?1",
                [id],
                map_probe_job,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn latest_probe_job_for_scope(
        &self,
        scope_key: &str,
    ) -> Result<Option<ProbeJob>, StoreError> {
        self.library
            .query_row(
                "SELECT id, kind, media_id, season, status, total, completed, succeeded,
                        failed, partial, error, created_at_ms, started_at_ms, finished_at_ms
                 FROM probe_jobs WHERE scope_key = ?1 ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
                [scope_key],
                map_probe_job,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn latest_marker_refresh_for_season(
        &self,
        media_id: &str,
        season: u32,
    ) -> Result<Option<ProbeJob>, StoreError> {
        self.library
            .query_row(
                "SELECT id, kind, media_id, season, status, total, completed, succeeded,
                        failed, partial, error, created_at_ms, started_at_ms, finished_at_ms
                 FROM probe_jobs
                 WHERE kind = 'marker_refresh' AND media_id = ?1
                   AND (season = ?2 OR season IS NULL)
                 ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
                params![media_id, season],
                map_probe_job,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn active_probe_job_for_scope(
        &self,
        scope_key: &str,
    ) -> Result<Option<ProbeJob>, StoreError> {
        let Some(job) = self.latest_probe_job_for_scope(scope_key)? else {
            return Ok(None);
        };
        Ok(job.is_active().then_some(job))
    }

    pub fn is_probe_queued(&self, ledger_id: &str) -> Result<bool, StoreError> {
        let active = self.library.query_row(
            "SELECT EXISTS(SELECT 1 FROM probe_job_units
             WHERE ledger_id = ?1 AND status IN ('queued', 'running'))",
            [ledger_id],
            |row| row.get(0),
        )?;
        Ok(active)
    }

    /// 这个台账行是否**曾经**被排入 probe（不区分当前状态）。
    ///
    /// `is_probe_queued` 只反映"此刻是否仍在队列里"，探测 worker 可能在调用方
    /// 观察之前就把它跑完；需要断言"扫描确实登记了探测"时用这个，避免把调度
    /// 时序当成业务结果。
    pub fn probe_unit_recorded_for_ledger(&self, ledger_id: &str) -> Result<bool, StoreError> {
        let recorded = self.library.query_row(
            "SELECT EXISTS(SELECT 1 FROM probe_job_units WHERE ledger_id = ?1)",
            [ledger_id],
            |row| row.get(0),
        )?;
        Ok(recorded)
    }

    pub fn active_probe_unit_for_ledger(
        &self,
        ledger_id: &str,
    ) -> Result<Option<ProbeJobUnit>, StoreError> {
        self.library
            .query_row(
                "SELECT job_id, ledger_id, kind, force_fingerprint,
                        reuse_fingerprint_cache, overwrite_markers, status, error,
                        sampling_plan_json, detection_outcome_json, reuse_media_info_cache, stage
                 FROM probe_job_units WHERE ledger_id = ?1 AND status IN ('queued', 'running')
                 ORDER BY rowid DESC LIMIT 1",
                [ledger_id],
                |row| {
                    Ok(ProbeJobUnit {
                        job_id: row.get(0)?,
                        ledger_id: row.get(1)?,
                        kind: row.get(2)?,
                        force_fingerprint: row.get(3)?,
                        reuse_fingerprint_cache: row.get(4)?,
                        overwrite_markers: row.get(5)?,
                        status: row.get(6)?,
                        error: row.get(7)?,
                        sampling_plan_json: row.get(8)?,
                        detection_outcome_json: row.get(9)?,
                        reuse_media_info_cache: row.get::<_, i64>(10).unwrap_or(0) != 0,
                        stage: row
                            .get::<_, String>(11)
                            .ok()
                            .and_then(|value| super::ProbeDispatchStage::from_str(&value))
                            .unwrap_or(super::ProbeDispatchStage::Metadata),
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }
}

fn map_probe_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProbeJob> {
    Ok(ProbeJob {
        id: row.get(0)?,
        kind: row.get(1)?,
        media_id: row.get(2)?,
        season: row.get::<_, Option<i64>>(3)?.map(|v| v as u32),
        status: row.get(4)?,
        total: row.get::<_, i64>(5)? as usize,
        completed: row.get::<_, i64>(6)? as usize,
        succeeded: row.get::<_, i64>(7)? as usize,
        failed: row.get::<_, i64>(8)? as usize,
        partial: row.get::<_, i64>(9)? as usize,
        error: row.get(10)?,
        created_at_ms: row.get(11)?,
        started_at_ms: row.get(12)?,
        finished_at_ms: row.get(13)?,
    })
}

fn is_constraint(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

/// Terminal status a probe job takes once every remaining unit is terminal.
///
/// `None` means the job must stay active. Every kind terminalizes as soon as
/// its last unit finishes, except `marker_refresh`, which only becomes
/// `succeeded` together with its marker results inside
/// `complete_marker_refresh` — unless a unit already failed.
///
/// Both the normal unit-completion path and ledger-retirement resync share this
/// decision so the two cannot disagree about when a job is done.
pub(crate) fn probe_job_terminal_status(
    kind: &str,
    total: usize,
    completed: usize,
    failed: usize,
    partial: usize,
    cancelled: usize,
) -> Option<&'static str> {
    if completed < total {
        return None;
    }
    if failed > 0 {
        return Some("failed");
    }
    match kind {
        "marker_refresh" => None,
        _ if cancelled > 0 => Some("cancelled"),
        _ if partial > 0 => Some("partial"),
        _ => Some("succeeded"),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}
