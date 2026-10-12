//! DB8 Task 3: batched retention for `playback_logs`.
//!
//! Only the subscribe database's playback history is pruned. Watch progress
//! and favorites (`playback_units`), live sessions (`playback_sessions`) and
//! QoE records (`playback_metrics`) are never touched: a row is a candidate
//! only when its own `ended_at` is strictly older than the cutoff.

use rusqlite::params;

use super::{Store, StoreError};

/// Outcome of one [`Store::prune_playback_logs`] batch.
///
/// * `scanned` / `candidates`: expired rows examined in this batch, capped at
///   `batch_size`. Both count the same rows because playback history has no
///   protection rule beyond the age cutoff.
/// * `deleted`: rows actually removed; `0` for `dry_run`.
/// * `protected`: always `0`. Rows inside the retention window are excluded by
///   the candidate predicate and are never scanned, so there is nothing to
///   report; the field exists for parity with the jobs retention batch.
/// * `has_more`: more expired rows remain after this batch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaybackRetentionBatch {
    pub scanned: usize,
    pub candidates: usize,
    pub deleted: usize,
    pub protected: usize,
    pub has_more: bool,
}

/// One DELETE statement stays well below SQLite's bound-parameter budget.
const DELETE_CHUNK: usize = 500;

impl Store {
    /// Delete at most `batch_size` `playback_logs` rows whose `ended_at` is
    /// strictly older than `cutoff`; `ended_at == cutoff` is kept.
    ///
    /// Candidate selection and deletion share one transaction on the subscribe
    /// connection, so a failure leaves the batch untouched and repeated calls
    /// are idempotent. `dry_run` performs the same bounded selection but writes
    /// nothing.
    pub fn prune_playback_logs(
        &self,
        cutoff: i64,
        batch_size: usize,
        dry_run: bool,
    ) -> Result<PlaybackRetentionBatch, StoreError> {
        if batch_size == 0 {
            return Err(StoreError::Invalid(
                "playback retention batch_size must be >= 1".into(),
            ));
        }
        self.playback_write(|store| store.prune_playback_logs_batch(cutoff, batch_size, dry_run))
    }

    fn prune_playback_logs_batch(
        &self,
        cutoff: i64,
        batch_size: usize,
        dry_run: bool,
    ) -> Result<PlaybackRetentionBatch, StoreError> {
        let ids = self.expired_log_ids(cutoff, batch_size)?;
        let has_more = ids.len() > batch_size;
        let ids = &ids[..ids.len().min(batch_size)];
        let deleted = if dry_run { 0 } else { self.delete_log_ids(ids)? };
        Ok(PlaybackRetentionBatch {
            scanned: ids.len(),
            candidates: ids.len(),
            deleted,
            protected: 0,
            has_more,
        })
    }

    /// Oldest expired log ids, capped at `batch_size + 1` so the caller can
    /// tell whether more candidates remain without counting the whole table.
    /// `started_at` order rides the existing `playback_logs_started_idx` and
    /// keeps the scan bounded; the age decision still uses `ended_at`.
    fn expired_log_ids(&self, cutoff: i64, batch_size: usize) -> Result<Vec<String>, StoreError> {
        let limit = i64::try_from(batch_size.saturating_add(1)).unwrap_or(i64::MAX);
        let mut stmt = self.subscribe.prepare(
            "SELECT id FROM playback_logs
             WHERE ended_at < ?1
             ORDER BY started_at ASC, id ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![cutoff, limit], |row| row.get(0))?;
        rows.collect::<Result<Vec<String>, _>>().map_err(Into::into)
    }

    fn delete_log_ids(&self, ids: &[String]) -> Result<usize, StoreError> {
        let mut deleted = 0;
        for chunk in ids.chunks(DELETE_CHUNK) {
            let marks = vec!["?"; chunk.len()].join(",");
            let sql = format!("DELETE FROM playback_logs WHERE id IN ({marks})");
            deleted += self
                .subscribe
                .execute(&sql, rusqlite::params_from_iter(chunk.iter().map(String::as_str)))?;
        }
        Ok(deleted)
    }
}
