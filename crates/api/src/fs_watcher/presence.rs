//! Watcher presence reconciliation: marks ledger rows missing instead of
//! automatically deleting ledger/Media/Playback facts.

use std::collections::HashMap;
use std::path::Path;

use store::{
    FilePresence, LedgerPresenceMode, LedgerPresenceObservation, LedgerPresenceResult,
    LibraryPathScope, StrictOwner,
};

use crate::library_presence::PresenceError;
use crate::management::ApiState;

fn collect_event_observations(
    state: &ApiState,
    path: &Path,
    scope: &LibraryPathScope,
    candidate_rows: Vec<domain::LedgerRow>,
    missing_map: &HashMap<String, i64>,
    media_kinds: &HashMap<domain::MediaId, domain::MediaKind>,
) -> HashMap<String, Vec<LedgerPresenceObservation>> {
    let mut observations_by_lib: HashMap<String, Vec<LedgerPresenceObservation>> = HashMap::new();

    for row in candidate_rows {
        let row_p = Path::new(&row.path);
        let matches_event = row_p == path || row_p.starts_with(path);
        if !matches_event {
            continue;
        }

        let Some(&real_kind) = media_kinds.get(&row.media_id) else {
            continue;
        };

        let Ok(resolved) = LibraryPathScope::resolve_path(row_p) else {
            continue;
        };

        let owner_lib = match scope.owner(&resolved, real_kind) {
            StrictOwner::Owned(lib) => lib,
            _ => continue,
        };

        if !owner_lib.realtime_watch || !scope.accepts_work(&owner_lib.id) {
            continue;
        }

        let matching_root = owner_lib
            .root_paths
            .iter()
            .find(|r| {
                resolved.starts_with(r)
                    || LibraryPathScope::resolve_path(r)
                        .is_ok_and(|res_r| resolved.starts_with(&res_r))
            });
        let Some(root) = matching_root else {
            continue;
        };
        if state.library_files.readable_root(root).is_err() {
            tracing::warn!(library_id = %owner_lib.id, root = %root.display(), "Root unreadable during watcher event");
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

        observations_by_lib
            .entry(owner_lib.id.clone())
            .or_default()
            .push(LedgerPresenceObservation {
                row: row.clone(),
                missing_at: missing_map.get(&row.path).copied(),
                presence,
            });
    }

    observations_by_lib
}

fn publish_event_observations(
    state: &ApiState,
    scope: &LibraryPathScope,
    observations_by_lib: HashMap<String, Vec<LedgerPresenceObservation>>,
) -> LedgerPresenceResult {
    let mut total_result = LedgerPresenceResult::default();
    let now = crate::job_loop::unix_now();

    for (lib_id, observations) in observations_by_lib {
        let lease = match state.acquire_library_work(&lib_id) {
            Ok(l) => l,
            Err(_) => continue,
        };

        let sub_res = lease.publish(|| {
            let store_arc = state.store();
            let guard = store_arc.lock();
            let current_snapshot = guard.library_ownership().map_err(|e| e.to_string())?;
            if !current_snapshot.accepts_work(&lib_id) {
                return Err("library is deleting".to_string());
            }
            if !scope.matches(&current_snapshot) {
                return Err("scope changed".to_string());
            }
            guard
                .apply_ledger_presence(&observations, LedgerPresenceMode::Verify, now)
                .map_err(|e| e.to_string())
        });

        match sub_res {
            Ok(res) => {
                total_result.missing += res.missing;
                total_result.restored += res.restored;
                total_result.deleted += res.deleted;
                total_result.skipped += res.skipped;
            }
            Err(err) => {
                tracing::warn!(library_id = %lib_id, %err, "Failed to publish watcher presence");
            }
        }
    }

    total_result
}

/// Observe a file event path and apply presence to matching active realtime libraries.
pub(crate) fn observe_event(
    state: &ApiState,
    path: &Path,
) -> Result<LedgerPresenceResult, PresenceError> {
    let (snapshot, candidate_rows, missing_map, media_kinds) = {
        let store_arc = state.store();
        let guard = store_arc.lock();
        let snapshot = guard.library_ownership()?;
        let rows = guard.list_ledger()?;
        let missing_map = guard.missing_at_by_path()?;
        let media_ids: Vec<domain::MediaId> = rows.iter().map(|r| r.media_id).collect();
        let media_map = guard.media_batch(&media_ids)?;
        let media_kinds: HashMap<domain::MediaId, domain::MediaKind> = media_map
            .into_iter()
            .map(|(mid, m)| (mid, m.kind))
            .collect();
        (snapshot, rows, missing_map, media_kinds)
    };

    let scope = LibraryPathScope::resolve(snapshot)?;
    let observations_by_lib = collect_event_observations(
        state,
        path,
        &scope,
        candidate_rows,
        &missing_map,
        &media_kinds,
    );

    if observations_by_lib.is_empty() {
        return Ok(LedgerPresenceResult::default());
    }

    Ok(publish_event_observations(state, &scope, observations_by_lib))
}
