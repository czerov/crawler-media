//! G-M5: single-instance lease on the data directory. One row; the owner
//! refreshes `heartbeat_at`, and a lease older than the TTL may be taken over.

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{Store, StoreError};

impl Store {
    /// Take the lease for `owner`. Returns `Some(other_owner)` when another
    /// instance holds a live lease (heartbeat within `ttl_secs`).
    pub fn try_acquire_instance_lock(
        &self,
        owner: &str,
        now: i64,
        ttl_secs: i64,
    ) -> Result<Option<String>, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(&self.app, TransactionBehavior::Immediate)?;
        let current: Option<(String, i64)> = tx
            .query_row(
                "SELECT owner, heartbeat_at FROM instance_lock WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((other, beat)) = current
            && other != owner
            && now - beat < ttl_secs
        {
            return Ok(Some(other));
        }
        tx.execute(
            "INSERT INTO instance_lock (id, owner, heartbeat_at) VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET owner = excluded.owner, heartbeat_at = excluded.heartbeat_at",
            params![owner, now],
        )?;
        tx.commit()?;
        Ok(None)
    }

    /// Refresh the lease. `false` means another instance took it over.
    pub fn heartbeat_instance_lock(&self, owner: &str, now: i64) -> Result<bool, StoreError> {
        let changed = self.app.execute(
            "UPDATE instance_lock SET heartbeat_at = ?2 WHERE id = 1 AND owner = ?1",
            params![owner, now],
        )?;
        Ok(changed == 1)
    }

    /// Drop the lease if `owner` still holds it (clean shutdown).
    pub fn release_instance_lock(&self, owner: &str) -> Result<(), StoreError> {
        self.app.execute(
            "DELETE FROM instance_lock WHERE id = 1 AND owner = ?1",
            [owner],
        )?;
        Ok(())
    }
}
