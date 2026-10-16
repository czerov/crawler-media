//! M6: Library files whose physical deletion must be retried. The ledger row
//! stays until the file is really gone.

use rusqlite::params;

use super::{Store, StoreError};

impl Store {
    /// Queue `path` for deletion (idempotent).
    pub fn enqueue_file_deletion(&self, path: &str, reason: &str) -> Result<(), StoreError> {
        self.library.execute(
            "INSERT INTO pending_file_deletions (path, reason) VALUES (?1, ?2)
             ON CONFLICT(path) DO NOTHING",
            params![path, reason],
        )?;
        Ok(())
    }

    /// Paths still waiting to be deleted, oldest first.
    pub fn list_pending_file_deletions(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .library
            .prepare("SELECT path FROM pending_file_deletions ORDER BY rowid")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Record a failed attempt with exponential backoff (capped at one day).
    pub fn record_file_deletion_failure(
        &self,
        path: &str,
        now: i64,
        error: &str,
    ) -> Result<(), StoreError> {
        self.library.execute(
            "UPDATE pending_file_deletions
             SET attempts = attempts + 1,
                 next_retry_at = ?2 + MIN(86400, 60 * (1 << MIN(attempts, 10))),
                 last_error = ?3
             WHERE path = ?1",
            params![path, now, error],
        )?;
        Ok(())
    }

    /// The file is gone: drop the queue row and its ledger row.
    pub fn complete_file_deletion(&self, path: &str) -> Result<(), StoreError> {
        self.delete_ledger_path(path)?;
        self.library
            .execute("DELETE FROM pending_file_deletions WHERE path = ?1", [path])?;
        Ok(())
    }
}
