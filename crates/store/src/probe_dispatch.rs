//! Bounded dispatch identity for durable probe work.
//!
//! `ProbeDispatchStage` classifies *which bounded worker window* a queued
//! `probe_job_units` row belongs to. It is not the same fact as the
//! `probe_stage_state` Metadata/Intro/Outro rows: those record the result of a
//! probe, while this stage only says where the pending unit is dispatched.
//!
//! The classification is persisted on the unit (`probe_job_units.stage`) and
//! derives from the job kind when the unit is created. A metadata result that
//! transitions a `marker_refresh` unit to fingerprint work writes the new stage
//! in the same database update, so a restart resumes from the durable state
//! instead of guessing from an in-memory work item.

use rusqlite::{OptionalExtension, params};

use super::{ProbeJob, ProbeJobUnit, Store, StoreError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProbeDispatchStage {
    Metadata,
    Fingerprint,
}

impl ProbeDispatchStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Fingerprint => "fingerprint",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "metadata" => Some(Self::Metadata),
            "fingerprint" => Some(Self::Fingerprint),
            _ => None,
        }
    }

    /// The stage a newly created unit starts in, derived from the job kind.
    pub(crate) fn for_job_kind(kind: &str) -> Self {
        if kind == "fingerprint_probe" {
            Self::Fingerprint
        } else {
            Self::Metadata
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProbeDispatchId {
    pub job_id: String,
    pub ledger_id: String,
    pub stage: ProbeDispatchStage,
}

/// One durable dispatch item loaded from a single ID.
///
/// The store loads exactly this row; it never materializes the whole pending
/// backlog. `source_version` is the metadata result's persisted source version
/// (empty when no metadata has been written yet).
#[derive(Clone, Debug)]
pub struct ProbeDispatchSnapshot {
    pub job: ProbeJob,
    pub unit: ProbeJobUnit,
    pub row: domain::LedgerRow,
    pub source_version: String,
}

impl Store {
    /// Pending dispatch IDs for one stage, newest work last, capped by `limit`.
    pub fn pending_probe_dispatch_ids(
        &self,
        stage: ProbeDispatchStage,
        limit: usize,
    ) -> Result<Vec<ProbeDispatchId>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut statement = self.library.prepare(
            "SELECT u.job_id, u.ledger_id
             FROM probe_job_units u
             JOIN probe_jobs j ON j.id = u.job_id
             WHERE u.stage = ?1 AND u.status = 'queued'
               AND j.status IN ('queued', 'running')
             ORDER BY j.created_at_ms, j.id, u.ledger_id
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![stage.as_str(), limit as i64], |row| {
            Ok(ProbeDispatchId {
                job_id: row.get(0)?,
                ledger_id: row.get(1)?,
                stage,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Load the job, unit, ledger row, and persisted source version for one ID.
    pub fn load_probe_dispatch_snapshot(
        &self,
        id: &ProbeDispatchId,
    ) -> Result<Option<ProbeDispatchSnapshot>, StoreError> {
        let unit = self
            .library
            .query_row(
                "SELECT u.job_id, u.ledger_id, u.kind, u.force_fingerprint,
                        u.reuse_fingerprint_cache, u.overwrite_markers, u.status, u.error,
                        u.sampling_plan_json, u.detection_outcome_json,
                        u.reuse_media_info_cache, u.stage
                 FROM probe_job_units u
                 JOIN probe_jobs j ON j.id = u.job_id
                 WHERE u.job_id = ?1 AND u.ledger_id = ?2",
                params![id.job_id, id.ledger_id],
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
                            .and_then(|value| ProbeDispatchStage::from_str(&value))
                            .unwrap_or(ProbeDispatchStage::Metadata),
                    })
                },
            )
            .optional()?;
        let Some(unit) = unit else {
            return Ok(None);
        };
        let Some(job) = self.get_probe_job(&id.job_id)? else {
            return Ok(None);
        };
        let Some(row) = self.get_ledger(&id.ledger_id)? else {
            return Ok(None);
        };
        let source_version = self
            .library
            .query_row(
                "SELECT source_version FROM file_meta WHERE ledger_id = ?1",
                params![id.ledger_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .unwrap_or_default();
        Ok(Some(ProbeDispatchSnapshot {
            job,
            unit,
            row,
            source_version,
        }))
    }

    /// Persist a completed metadata result as fingerprint work on the same unit.
    ///
    /// The unit returns to `queued` so the bounded fingerprint window can claim
    /// it; a crash between the metadata result and this transition leaves the
    /// unit running, and startup recovery requeues it at the metadata stage.
    pub fn advance_probe_unit_to_fingerprint(
        &self,
        job_id: &str,
        ledger_id: &str,
    ) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "UPDATE probe_job_units SET status = 'queued', stage = 'fingerprint'
             WHERE job_id = ?1 AND ledger_id = ?2
               AND status IN ('running', 'queued') AND stage = 'metadata'",
            params![job_id, ledger_id],
        )?;
        Ok(changed > 0)
    }
}
