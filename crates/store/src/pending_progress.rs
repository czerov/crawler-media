//! H3 stalled timeout: track when a pending download last made progress.

use rusqlite::{OptionalExtension, params};

use domain::SubscribeId;

use super::{Store, StoreError};

impl Store {
    /// Record an observed `progress` (0..=1). `last_progress_at` only moves
    /// when progress increased. Returns the time progress was last seen to
    /// change (falls back to `submitted_at`), or `None` for unknown rows.
    pub fn touch_pending_progress(
        &self,
        subscribe_id: SubscribeId,
        enclosure: &str,
        progress: f64,
        now: i64,
    ) -> Result<Option<i64>, StoreError> {
        let id = subscribe_id.to_string();
        self.subscribe.execute(
            "UPDATE pending_downloads SET last_progress = ?3, last_progress_at = ?4
             WHERE subscribe_id = ?1 AND enclosure = ?2 AND ?3 > COALESCE(last_progress, 0.0) + 1e-9",
            params![id, enclosure, progress, now],
        )?;
        Ok(self
            .subscribe
            .query_row(
                "SELECT COALESCE(last_progress_at, NULLIF(submitted_at, 0)) FROM pending_downloads
                 WHERE subscribe_id = ?1 AND enclosure = ?2",
                params![id, enclosure],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Release an active pending as `stalled`: it no longer holds its slot and
    /// automatic rounds never pick the enclosure again.
    pub fn mark_pending_stalled(
        &self,
        subscribe_id: SubscribeId,
        enclosure: &str,
    ) -> Result<(), StoreError> {
        self.subscribe.execute(
            "UPDATE pending_downloads SET state = 'stalled'
             WHERE subscribe_id = ?1 AND enclosure = ?2 AND state = 'active'",
            params![subscribe_id.to_string(), enclosure],
        )?;
        Ok(())
    }

    /// Review N-1: one more round in which the source could not be probed.
    /// Returns `(attempts, first_seen)` for the row, `None` for unknown rows.
    pub fn note_probe_wait(
        &self,
        subscribe_id: SubscribeId,
        enclosure: &str,
        now: i64,
    ) -> Result<Option<(i64, i64)>, StoreError> {
        let id = subscribe_id.to_string();
        self.subscribe.execute(
            "UPDATE pending_downloads
             SET probe_wait_attempts = COALESCE(probe_wait_attempts, 0) + 1,
                 probe_wait_since = COALESCE(probe_wait_since, ?3)
             WHERE subscribe_id = ?1 AND enclosure = ?2 AND state = 'active'",
            params![id, enclosure, now],
        )?;
        Ok(self
            .subscribe
            .query_row(
                "SELECT probe_wait_attempts, probe_wait_since FROM pending_downloads
                 WHERE subscribe_id = ?1 AND enclosure = ?2 AND state = 'active'",
                params![id, enclosure],
                |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()?
            .and_then(|(a, s)| Some((a?, s?))))
    }

    /// D-M3: remember the info hash the downloader assigned to a pending row.
    pub fn set_pending_info_hash(
        &self,
        subscribe_id: SubscribeId,
        enclosure: &str,
        hash: &str,
    ) -> Result<(), StoreError> {
        self.subscribe.execute(
            "UPDATE pending_downloads SET info_hash = ?3 WHERE subscribe_id = ?1 AND enclosure = ?2",
            params![subscribe_id.to_string(), enclosure, hash.to_ascii_lowercase()],
        )?;
        Ok(())
    }

    /// D-M3: enclosure → info hash for the Subscribe's active pending rows.
    pub fn load_pending_info_hashes(
        &self,
        subscribe_id: SubscribeId,
    ) -> Result<std::collections::HashMap<String, String>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT enclosure, info_hash FROM pending_downloads
             WHERE subscribe_id = ?1 AND state = 'active' AND info_hash IS NOT NULL",
        )?;
        let rows = stmt.query_map([subscribe_id.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// F-1: episodes (`"s:e,s:e"`) still to keep once the file list is known;
    /// `None` clears the deferred selection.
    pub fn set_pending_selective(
        &self,
        subscribe_id: SubscribeId,
        enclosure: &str,
        missing: Option<&str>,
    ) -> Result<(), StoreError> {
        self.subscribe.execute(
            "UPDATE pending_downloads SET selective_missing = ?3 WHERE subscribe_id = ?1 AND enclosure = ?2",
            params![subscribe_id.to_string(), enclosure, missing],
        )?;
        Ok(())
    }

    /// F-1: enclosure → deferred missing episodes for active pending rows.
    pub fn load_pending_selective(
        &self,
        subscribe_id: SubscribeId,
    ) -> Result<std::collections::HashMap<String, String>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT enclosure, selective_missing FROM pending_downloads
             WHERE subscribe_id = ?1 AND state = 'active' AND selective_missing IS NOT NULL",
        )?;
        let rows = stmt.query_map([subscribe_id.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}
