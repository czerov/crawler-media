//! Library file reading trait and presence reconciliation.
//!
//! Provides `LibraryFileRead` abstraction (production and mock/fake)
//! and orchestrates verify and prune operations across bounded blocking work,
//! lease publication barrier, Store checks, and probe cancellation.

use std::path::Path;
use std::sync::Arc;

use domain::MediaId;
use store::{
    FilePresence, LedgerPresenceMode, LedgerPresenceObservation, LedgerPresenceResult,
    LibraryPathScope, StoreError, StrictOwner,
};

use crate::management::ApiState;

pub trait LibraryFileRead: Send + Sync {
    /// Minimum health check for a library root: exists, is a directory, readable.
    fn readable_root(&self, path: &Path) -> std::io::Result<()>;
    /// Check existence of a single file. Only explicit NotFound converts to Ok(false).
    fn presence(&self, path: &Path) -> std::io::Result<bool>;
}

#[derive(Default)]
pub struct SystemLibraryFileRead;

impl LibraryFileRead for SystemLibraryFileRead {
    fn readable_root(&self, path: &Path) -> std::io::Result<()> {
        let meta = std::fs::metadata(path)?;
        if !meta.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "root is not a directory",
            ));
        }
        // Try opening directory read
        let mut reader = std::fs::read_dir(path)?;
        // Fetch first entry or end of empty dir
        let _ = reader.next();
        Ok(())
    }

    fn presence(&self, path: &Path) -> std::io::Result<bool> {
        match std::fs::metadata(path) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
}

#[derive(Debug)]
pub enum PresenceError {
    Missing,
    Deleting,
    Ambiguous,
    RootUnavailable,
    ScopeChanged,
    Busy,
    Store(StoreError),
}

impl From<StoreError> for PresenceError {
    fn from(err: StoreError) -> Self {
        Self::Store(err)
    }
}

pub(crate) fn verify(
    state: &ApiState,
    library_id: &str,
) -> Result<LedgerPresenceResult, PresenceError> {
    reconcile_presence(state, library_id, None, LedgerPresenceMode::Verify)
}

pub(crate) fn prune(
    state: &ApiState,
    library_id: &str,
    media_id: Option<MediaId>,
) -> Result<LedgerPresenceResult, PresenceError> {
    reconcile_presence(state, library_id, media_id, LedgerPresenceMode::Prune)
}

fn reconcile_presence(
    state: &ApiState,
    library_id: &str,
    target_media_id: Option<MediaId>,
    mode: LedgerPresenceMode,
) -> Result<LedgerPresenceResult, PresenceError> {
    // 1. In Store lock, take Library configuration, removal state, and snapshot.
    let (target_lib, snapshot, missing_map) = {
        let guard = state.store.lock();
        let snapshot = guard.library_ownership()?;
        let lib = snapshot
            .library(library_id)
            .cloned()
            .ok_or(PresenceError::Missing)?;
        if !snapshot.accepts_work(library_id) {
            return Err(PresenceError::Deleting);
        }
        let missing_map = guard.missing_at_by_path()?;
        (lib, snapshot, missing_map)
    };

    // 2. Resolve scope out of lock
    let scope = LibraryPathScope::resolve(snapshot)?;
    if !scope.accepts_work(library_id) {
        return Err(PresenceError::Deleting);
    }

    // Check roots readability
    for root in &target_lib.root_paths {
        if let Err(e) = state.library_files.readable_root(root) {
            tracing::warn!(root = %root.display(), error = %e, "Library root unavailable");
            return Err(PresenceError::RootUnavailable);
        }
    }

    // Collect candidates for this library
    let (candidate_rows, candidate_scope) = {
        let guard = state.store.lock();
        let rows = guard.list_ledger()?;
        (rows, scope.clone())
    };

    let mut observations = Vec::new();

    for row in candidate_rows {
        if let Some(mid) = target_media_id {
            if row.media_id != mid {
                continue;
            }
        }

        let Ok(resolved) = LibraryPathScope::resolve_path(Path::new(&row.path)) else {
            observations.push(LedgerPresenceObservation {
                row: row.clone(),
                missing_at: missing_map.get(&row.path).copied(),
                presence: FilePresence::Unknown,
            });
            continue;
        };

        match candidate_scope.owner(&resolved, target_lib.kind) {
            StrictOwner::Owned(owner) if owner.id == library_id => {
                // Eligible row
            }
            StrictOwner::Ambiguous => return Err(PresenceError::Ambiguous),
            _ => continue,
        }

        let presence = match state.library_files.presence(Path::new(&row.path)) {
            Ok(true) => FilePresence::Present,
            Ok(false) => FilePresence::Missing,
            Err(e) => {
                tracing::warn!(path = %row.path, error = %e, "Observing file presence failed");
                FilePresence::Unknown
            }
        };

        observations.push(LedgerPresenceObservation {
            row: row.clone(),
            missing_at: missing_map.get(&row.path).copied(),
            presence,
        });
    }

    // Cancel active probe work if in Prune mode
    if mode == LedgerPresenceMode::Prune {
        let ids_to_cancel: Vec<String> = observations
            .iter()
            .filter(|obs| obs.presence == FilePresence::Missing && obs.missing_at.is_some())
            .map(|obs| obs.row.id.to_string())
            .collect();
        if !ids_to_cancel.is_empty() {
            let _ = state
                .probe
                .cancel_ledger_work(&ids_to_cancel, "pruning missing files");
        }
    }

    // 3. Acquire lease: Registry -> Store
    let lease = match state.acquire_library_work(library_id) {
        Ok(lease) => lease,
        Err(_) => {
            return Err(PresenceError::Deleting);
        }
    };

    // 4. In publication barrier, verify presence one last time for拟删除 rows,
    // check scope matches, and apply presence in a single Immediate transaction.
    lease.publish(|| {
        let guard = state.store.lock();
        let current_snapshot = guard.library_ownership().map_err(|e| e.to_string())?;
        if !current_snapshot.accepts_work(library_id) {
            return Err("library is deleting".to_string());
        }
        if !candidate_scope.matches(&current_snapshot) {
            return Err("library scope changed".to_string());
        }

        let now = crate::job_loop::unix_now();
        guard
            .apply_ledger_presence(&observations, mode, now)
            .map_err(|e| format!("store: {}", e))
    })
    .map_err(|err_str| {
        if err_str.contains("library is deleting") {
            PresenceError::Deleting
        } else {
            PresenceError::Store(StoreError::Sqlite(rusqlite::Error::ToSqlConversionFailure(
                Box::new(std::io::Error::other(err_str)),
            )))
        }
    })
}
