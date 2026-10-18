//! M6: Library files whose physical deletion must be retried. The ledger row
//! stays until the file is really gone.

use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError};

/// Review L-10: after this many failed attempts a deletion is no longer
/// retried automatically; it stays listed (with its last error) for the user.
pub const MAX_FILE_DELETION_ATTEMPTS: i64 = 20;

/// Review M-3: a due deletion together with the identity recorded when it
/// was queued. The file is only deleted while that identity still holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedFileDeletion {
    pub path: String,
    pub ledger_id: Option<String>,
    pub file_size: Option<u64>,
    pub file_inode: Option<u64>,
}

#[cfg(unix)]
fn inode_of(meta: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.ino())
}

#[cfg(not(unix))]
fn inode_of(_meta: &std::fs::Metadata) -> Option<u64> {
    None
}

/// Size and inode of `path` right now (`None` when it cannot be read).
pub fn file_identity(path: &str) -> Option<(u64, Option<u64>)> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    Some((meta.len(), inode_of(&meta)))
}

impl Store {
    /// Queue `path` for deletion (idempotent). Records the ledger id and the
    /// file's size/inode so a later retry never deletes a different file that
    /// was placed at the same path (review M-3).
    pub fn enqueue_file_deletion(&self, path: &str, reason: &str) -> Result<(), StoreError> {
        let ledger_id: Option<String> = self
            .library
            .query_row("SELECT id FROM ledger WHERE path = ?1", [path], |row| {
                row.get(0)
            })
            .optional()?;
        let identity = file_identity(path);
        self.library.execute(
            "INSERT INTO pending_file_deletions (path, reason, ledger_id, file_size, file_inode)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO NOTHING",
            params![
                path,
                reason,
                ledger_id,
                identity.map(|(size, _)| size as i64),
                identity.and_then(|(_, ino)| ino).map(|ino| ino as i64),
            ],
        )?;
        Ok(())
    }

    /// Review M-3: deletions whose backoff has expired (`next_retry_at <= now`).
    pub fn list_due_file_deletions(&self, now: i64) -> Result<Vec<QueuedFileDeletion>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT path, ledger_id, file_size, file_inode FROM pending_file_deletions
             WHERE next_retry_at <= ?1 AND attempts < ?2 ORDER BY rowid",
        )?;
        let rows = stmt.query_map(params![now, MAX_FILE_DELETION_ATTEMPTS], |row| {
            Ok(QueuedFileDeletion {
                path: row.get(0)?,
                ledger_id: row.get(1)?,
                file_size: row.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                file_inode: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The queued identity no longer matches what is at the path (the path
    /// was reused): drop the queue row and keep the file and its ledger row.
    pub fn abandon_file_deletion(&self, path: &str) -> Result<(), StoreError> {
        self.library
            .execute("DELETE FROM pending_file_deletions WHERE path = ?1", [path])?;
        Ok(())
    }

    /// Review L-10: deletions that exhausted their retries, with last error.
    pub fn list_stuck_file_deletions(&self) -> Result<Vec<(String, Option<String>)>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT path, last_error FROM pending_file_deletions
             WHERE attempts >= ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map([MAX_FILE_DELETION_ATTEMPTS], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Make a queued deletion due now (manual "retry now").
    pub fn reset_file_deletion_backoff(&self, path: &str) -> Result<(), StoreError> {
        self.library.execute(
            "UPDATE pending_file_deletions SET next_retry_at = 0, attempts = 0 WHERE path = ?1",
            [path],
        )?;
        Ok(())
    }

    /// Current ledger id at `path`, if any.
    pub fn ledger_id_at_path(&self, path: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .library
            .query_row("SELECT id FROM ledger WHERE path = ?1", [path], |row| {
                row.get(0)
            })
            .optional()?)
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
