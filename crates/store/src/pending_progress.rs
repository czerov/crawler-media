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
}
