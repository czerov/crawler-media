//! Watcher presence reconciliation: marks ledger rows missing instead of
//! automatically deleting ledger/Media/Playback facts.

use std::collections::HashMap;
use std::path::Path;

use store::{
    FilePresence, LedgerPresenceMode, LedgerPresenceObservation, LedgerPresenceResult,
    LibraryPathScope, StrictOwner,
};

use crate::library_presence::{PresenceError, PresencePublishError, publish_presence};
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
            Err(error) => {
                tracing::warn!(%error, library_id = %lib_id, "获取 Library 工作租约失败，跳过本次 watcher presence 发布");
                continue;
            }
        };

        let sub_res = publish_presence(&lease, || {
            let store_arc = state.store();
            let guard = store_arc.lock();
            let current_snapshot = guard.library_ownership()?;
            if !current_snapshot.accepts_work(&lib_id) {
                return Err(PresencePublishError::LibraryDeleting);
            }
            if !scope.matches(&current_snapshot) {
                return Err(PresencePublishError::ScopeChanged);
            }
            guard
                .apply_ledger_presence(&observations, LedgerPresenceMode::Verify, now)
                .map_err(PresencePublishError::from)
        });

        match sub_res {
            Ok(res) => {
                total_result.missing += res.missing;
                total_result.restored += res.restored;
                total_result.deleted += res.deleted;
                total_result.skipped += res.skipped;
            }
            Err(error) => {
                // 「library 正在删除 / scope 变化」是 watcher 的预期竞争：管理员冻结
                // Library 时 watcher 可能正在发布旧观察，这不是故障，不能打成 ERROR。
                if is_expected_presence_race(&error) {
                    tracing::debug!(library_id = %lib_id, error = %error, "watcher presence 发布被生命周期/scope 变化取消");
                } else {
                    tracing::error!(library_id = %lib_id, error = %error, "发布 watcher presence 失败");
                }
            }
        }
    }

    total_result
}

/// 预期竞争（Library 冻结/删除、scope 换代、租约取消）与真实故障的区分：
/// 按类型判定，不匹配文案 —— 改文案不会把真实故障变成"预期竞争"。
fn is_expected_presence_race(error: &PresenceError) -> bool {
    matches!(error, PresenceError::Deleting | PresenceError::ScopeChanged)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实 Store 故障即使文案里带 `scope changed`，也不能被当成预期竞争：
    /// 旧实现按 `contains` 分类时，改一条错误文案就会把故障降级成 debug。
    #[test]
    fn store_failure_is_not_an_expected_race_even_when_text_looks_like_one() {
        assert!(!is_expected_presence_race(&PresenceError::Store(
            store::StoreError::Invalid("scope changed".into())
        )));
        assert!(!is_expected_presence_race(&PresenceError::Busy));
        assert!(!is_expected_presence_race(&PresenceError::Missing));
        assert!(is_expected_presence_race(&PresenceError::Deleting));
        assert!(is_expected_presence_race(&PresenceError::ScopeChanged));
    }
}
