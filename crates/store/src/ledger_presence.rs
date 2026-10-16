//! Ledger presence types and transactional updates for safe verification and pruning.

use domain::{LedgerId, LedgerRow, MediaId};
use rusqlite::Transaction;

use crate::StoreError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilePresence {
    Present,
    Missing,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct LedgerPresenceObservation {
    pub row: LedgerRow,
    pub missing_at: Option<i64>,
    pub presence: FilePresence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedgerPresenceMode {
    Verify,
    Prune,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LedgerPresenceResult {
    pub missing: usize,
    pub restored: usize,
    pub deleted: usize,
    pub skipped: usize,
}

impl crate::Store {
    /// Apply presence observations within a single Immediate transaction in library.db.
    ///
    /// For each observation, checks whether the current database row still matches
    /// the observed identity (id, path, media_id, and missing_at). If it changed,
    /// the row is skipped.
    ///
    /// In Verify mode:
    /// - Missing: sets missing_at = now if not already set.
    /// - Present: clears missing_at if set.
    /// - Unknown: no change.
    ///
    /// In Prune mode:
    /// - Missing: if it was already marked missing (missing_at is Some), deletes
    ///   the ledger row and file-derived rows via `delete_ledger_in_tx`.
    /// - Present: clears missing_at if set.
    /// - Unknown: no change.
    pub fn apply_ledger_presence(
        &self,
        observations: &[LedgerPresenceObservation],
        mode: LedgerPresenceMode,
        now: i64,
    ) -> Result<LedgerPresenceResult, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;

        let mut result = LedgerPresenceResult::default();

        for obs in observations {
            let current: Option<(String, String, Option<i64>, bool)> = tx
                .query_row(
                    "SELECT path, media_id, missing_at, transfer_pending FROM ledger WHERE id = ?1",
                    [&obs.row.id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .rusqlite_optional()?;

            let Some((cur_path, cur_media_id, cur_missing_at, pending)) = current else {
                result.skipped += 1;
                continue;
            };

            // Check identity
            if cur_path != obs.row.path
                || cur_media_id != obs.row.media_id.to_string()
                || cur_missing_at != obs.missing_at
            {
                result.skipped += 1;
                continue;
            }
            if pending {
                if obs.presence != FilePresence::Present {
                    result.skipped += 1;
                    continue;
                }
                tx.execute("UPDATE ledger SET transfer_pending = 0 WHERE id = ?1",
                    [obs.row.id.to_string()])?;
                tracing::info!(ledger_id = %obs.row.id, path = %obs.row.path,
                    "Recovered completed Transfer publication");
            }

            match mode {
                LedgerPresenceMode::Verify => match obs.presence {
                    FilePresence::Missing => {
                        if cur_missing_at.is_none() {
                            tx.execute(
                                "UPDATE ledger SET missing_at = ?1 WHERE id = ?2",
                                rusqlite::params![now, obs.row.id.to_string()],
                            )?;
                            result.missing += 1;
                        }
                    }
                    FilePresence::Present => {
                        if cur_missing_at.is_some() {
                            tx.execute(
                                "UPDATE ledger SET missing_at = NULL WHERE id = ?1",
                                rusqlite::params![obs.row.id.to_string()],
                            )?;
                            result.restored += 1;
                        }
                    }
                    FilePresence::Unknown => {
                        result.skipped += 1;
                    }
                },
                LedgerPresenceMode::Prune => match obs.presence {
                    FilePresence::Missing => {
                        if cur_missing_at.is_some() {
                            crate::ledger_cleanup::delete_ledger_in_tx(&tx, obs.row.id)?;
                            result.deleted += 1;
                        } else {
                            result.skipped += 1;
                        }
                    }
                    FilePresence::Present => {
                        if cur_missing_at.is_some() {
                            tx.execute(
                                "UPDATE ledger SET missing_at = NULL WHERE id = ?1",
                                rusqlite::params![obs.row.id.to_string()],
                            )?;
                            result.restored += 1;
                        }
                    }
                    FilePresence::Unknown => {
                        result.skipped += 1;
                    }
                },
            }
        }

        tx.commit()?;
        Ok(result)
    }
}

trait RusqliteOptionalExt<T> {
    fn rusqlite_optional(self) -> Result<Option<T>, StoreError>;
}

impl<T> RusqliteOptionalExt<T> for Result<T, rusqlite::Error> {
    fn rusqlite_optional(self) -> Result<Option<T>, StoreError> {
        match self {
            Ok(val) => Ok(Some(val)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }
}
