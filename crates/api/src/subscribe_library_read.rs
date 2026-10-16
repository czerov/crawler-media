use std::path::Path;

use domain::{MediaKind, Subscribe};
use store::{LibraryPathScope, Store, StoreError, StrictOwner, SubscribeLibrarySnapshot};
use subscribe::{QualityFact, SubscribeFacts};

use crate::management::ApiState;

/// Evaluate SubscribeFacts out-of-lock using an owned DB snapshot.
///
/// Pure path-scope resolution and file verification run outside the Store lock.
pub fn evaluate_subscribe_facts_out_of_lock(
    snapshot: &SubscribeLibrarySnapshot,
) -> Result<SubscribeFacts, StoreError> {
    evaluate_subscribe_facts_out_of_lock_with(snapshot, &crate::browse_file_read::LocalBrowseFileRead)
}

/// Evaluate SubscribeFacts out-of-lock using an owned DB snapshot and a file existence seam.
pub fn evaluate_subscribe_facts_out_of_lock_with(
    snapshot: &SubscribeLibrarySnapshot,
    files: &dyn crate::browse_file_read::BrowseFileRead,
) -> Result<SubscribeFacts, StoreError> {
    let target_library_id = match snapshot.subscribe.library_id {
        Some(id) => id.to_string(),
        None => snapshot
            .ownership
            .default_for(snapshot.kind)
            .map(|lib| lib.id.clone())
            .ok_or_else(|| StoreError::Missing("target Library".into()))?,
    };

    let target_library = snapshot
        .ownership
        .library(&target_library_id)
        .ok_or_else(|| StoreError::Missing("target Library".into()))?;

    if target_library.kind != snapshot.kind {
        return Err(StoreError::Protected(
            "target Library kind does not match Media".into(),
        ));
    }

    let scope = LibraryPathScope::resolve(snapshot.ownership.clone())?;

    let mut facts = SubscribeFacts::default();

    for ((season, episode), fact) in snapshot.saved.entries() {
        let Some(path_str) = fact.path.as_deref() else {
            continue;
        };
        let p = Path::new(path_str);
        let resolved = match LibraryPathScope::resolve_path(p) {
            Ok(r) => r,
            Err(e) => return Err(e),
        };

        match scope.owner(&resolved, snapshot.kind) {
            StrictOwner::Owned(owner) if owner.id == target_library.id => {
                facts.replace(season, episode, fact.clone());
                if let Some(quality) = snapshot.saved.quality(path_str) {
                    facts.set_quality(path_str.to_string(), quality.clone());
                }
            }
            _ => {
                tracing::info!(
                    subscribe_id = %snapshot.subscribe.id,
                    library_id = %target_library.id,
                    season,
                    episode,
                    path = path_str,
                    "已保存事实不属于目标 Library，本次不把它当作已拥有内容"
                );
            }
        }
    }

    for row in &snapshot.rows {
        let p = Path::new(&row.path);
        if !files.is_file(p) {
            continue;
        }

        let resolved = match LibraryPathScope::resolve_path(p) {
            Ok(r) => r,
            Err(e) => return Err(e),
        };

        if !matches!(scope.owner(&resolved, snapshot.kind), StrictOwner::Owned(owner) if owner.id == target_library.id)
        {
            continue;
        }

        let fact = QualityFact {
            score: row.filter_score.unwrap_or(0),
            path: Some(row.path.clone()),
        };
        let key = (row.season, row.episode);

        // Check if older path for this key is missing from snapshot rows
        let stale = facts
            .get(key.0, key.1)
            .and_then(|f| f.path.as_deref())
            .map(|old| !snapshot.rows.iter().any(|r| r.path == old))
            .unwrap_or(false);

        if stale {
            facts.replace(key.0, key.1, fact);
        } else {
            facts.upsert(key.0, key.1, fact);
        }

        if let Some(quality_res) = snapshot.qualities.get(&row.id) {
            match quality_res {
                Ok(quality) => facts.set_quality(row.path.clone(), quality.clone()),
                Err(e) => return Err(StoreError::Invalid(format!("corrupt persisted quality: {e}"))),
            }
        }
    }

    let mut scoped = SubscribeFacts::default();
    for ((season, episode), fact) in facts.entries() {
        scoped.replace(season, episode, fact.clone());
        if let Some(path) = fact.path.as_deref() {
            if let Some(quality) = facts.quality(path) {
                scoped.set_quality(path.to_string(), quality.clone());
            }
        }
    }

    Ok(scoped)
}

/// An observed Subscribe facts result alongside the DB revisions at the time of observation.
#[derive(Clone, Debug)]
pub struct SubscribeLibraryObservation {
    pub facts: SubscribeFacts,
    pub revisions: store::ReadRevisions,
    pub library_id: String,
    pub library_root: std::path::PathBuf,
    pub ownership: store::LibraryOwnership,
}

/// Read subscribe library facts and observation context out of the Store lock.
pub fn observe_subscribe_library_facts(
    state: &ApiState,
    subscribe: &Subscribe,
    kind: MediaKind,
) -> Result<SubscribeLibraryObservation, String> {
    let snapshot = {
        let store = state.store.lock();
        store
            .subscribe_library_snapshot(subscribe, kind)
            .map_err(|e| e.to_string())?
    };

    let target_library_id = match snapshot.subscribe.library_id {
        Some(id) => id.to_string(),
        None => snapshot
            .ownership
            .default_for(snapshot.kind)
            .map(|lib| lib.id.clone())
            .ok_or_else(|| "目标 Library 不存在".to_string())?,
    };

    let target_library = snapshot
        .ownership
        .library(&target_library_id)
        .ok_or_else(|| "目标 Library 不存在".to_string())?;

    let library_root = target_library
        .root_paths
        .first()
        .cloned()
        .unwrap_or_default();

    let facts = evaluate_subscribe_facts_out_of_lock_with(&snapshot, state.browse_files.as_ref())
        .map_err(|e| e.to_string())?;

    Ok(SubscribeLibraryObservation {
        facts,
        revisions: snapshot.revisions,
        library_id: target_library_id,
        library_root,
        ownership: snapshot.ownership,
    })
}

/// Read subscribe library facts out of the Store lock.
///
/// Synchronous function; callers are already running in a blocking worker or background task.
/// Takes one owned DB snapshot under the lock and resolves path-scope/files outside the lock.
pub fn read_subscribe_library_facts(
    state: &ApiState,
    subscribe: &Subscribe,
    kind: MediaKind,
) -> Result<SubscribeFacts, String> {
    observe_subscribe_library_facts(state, subscribe, kind).map(|obs| obs.facts)
}
