//! Watcher presence reconciliation: marks ledger rows missing instead of
//! automatically deleting ledger/Media/Playback facts.

use std::path::Path;

use store::{
    FilePresence, LedgerPresenceMode, LedgerPresenceObservation, LedgerPresenceResult,
    LibraryPathScope, StrictOwner,
};

use crate::library_presence::PresenceError;
use crate::management::ApiState;

/// Observe a file event path and apply presence to matching active realtime libraries.
///
/// This function never deletes ledger rows or user facts.
/// Missing files get `missing_at` set (only on first observation);
/// restored files have `missing_at` cleared.
pub(crate) fn observe_event(
    state: &ApiState,
    path: &Path,
) -> Result<LedgerPresenceResult, PresenceError> {
    // 1. Snapshot libraries in store lock
    let (snapshot, candidate_rows, missing_map) = {
        let store_arc = state.store();
        let guard = store_arc.lock();
        let snapshot = guard.library_ownership()?;
        let rows = guard.list_ledger()?;
        let missing_map = guard.missing_at_by_path()?;
        (snapshot, rows, missing_map)
    };

    // 2. Resolve scope out of lock
    let scope = LibraryPathScope::resolve(snapshot.clone())?;

    let mut observations = Vec::new();

    // Check matching candidate rows
    for row in candidate_rows {
        let row_p = Path::new(&row.path);
        let matches_event = row_p == path || row_p.starts_with(path);
        if !matches_event {
            continue;
        }

        let Ok(resolved) = LibraryPathScope::resolve_path(row_p) else {
            observations.push(LedgerPresenceObservation {
                row: row.clone(),
                missing_at: missing_map.get(&row.path).copied(),
                presence: FilePresence::Unknown,
            });
            continue;
        };

        // Match owner library for this row
        let owner_lib = match scope.owner(&resolved, domain::MediaKind::Movie) {
            StrictOwner::Owned(lib) => lib,
            _ => match scope.owner(&resolved, domain::MediaKind::Tv) {
                StrictOwner::Owned(lib) => lib,
                _ => continue,
            },
        };

        // Must be active and realtime_watch == true
        if !owner_lib.realtime_watch || !scope.accepts_work(&owner_lib.id) {
            continue;
        }

        // Check if root is readable
        let root_ok = owner_lib
            .root_paths
            .iter()
            .any(|r| state.library_files.readable_root(r).is_ok());
        if !root_ok {
            tracing::warn!(library_id = %owner_lib.id, "Root unreadable during watcher event");
            continue;
        }

        let presence = match state.library_files.presence(row_p) {
            Ok(true) => FilePresence::Present,
            Ok(false) => FilePresence::Missing,
            Err(e) => {
                tracing::warn!(path = %row.path, error = %e, "Watcher file presence failed");
                FilePresence::Unknown
            }
        };

        observations.push(LedgerPresenceObservation {
            row: row.clone(),
            missing_at: missing_map.get(&row.path).copied(),
            presence,
        });
    }

    if observations.is_empty() {
        return Ok(LedgerPresenceResult::default());
    }

    let now = crate::job_loop::unix_now();
    let store_arc = state.store();
    let guard = store_arc.lock();
    Ok(guard.apply_ledger_presence(&observations, LedgerPresenceMode::Verify, now)?)
}
