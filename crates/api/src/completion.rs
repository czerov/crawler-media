//! M4a: `completed` tracking state for bounded, non-wash subscriptions.
//! A completed subscription keeps its search JobDef, but the search worker
//! re-validates the library first and skips the indexers while it stays
//! complete; a lost file or widened range makes it `active` again.

use domain::{Coverage, Subscribe};

use crate::ApiState;

pub(crate) const COMPLETED: &str = "completed";

/// Every unit of a bounded, non-wash coverage owns a file that still exists.
pub(crate) fn library_complete(state: &ApiState, subscribe: &Subscribe) -> bool {
    if subscribe.wash_cut
        || matches!(
            subscribe.coverage,
            Coverage::Tv {
                episode_to: None,
                ..
            }
        )
    {
        return false;
    }
    let Ok(facts) = state.store.lock().load_subscribe_facts(subscribe.id) else {
        return false;
    };
    let units = subscribe.coverage.units();
    !units.is_empty()
        && units.iter().all(|(season, episode)| {
            facts
                .get(*season, *episode)
                .and_then(|fact| fact.path.as_deref())
                .is_some_and(|path| std::path::Path::new(path).exists())
        })
}

fn set_state(state: &ApiState, subscribe: &Subscribe, value: &str) {
    let store = state.store.lock();
    let Ok(Some(mut current)) = store.get_subscribe(subscribe.id) else {
        return;
    };
    current.tracking_state = value.to_string();
    if let Err(error) = store.update_subscribe(&current) {
        tracing::warn!(subscribe_id = %subscribe.id, %error, "更新订阅追踪状态失败");
    }
}

/// After Transfer / search: an active subscription that is fully in the
/// library becomes completed.
pub(crate) fn mark_if_complete(state: &ApiState, subscribe: &Subscribe) {
    let active = state
        .store
        .lock()
        .get_subscribe(subscribe.id)
        .ok()
        .flatten()
        .is_some_and(|s| s.tracking_state == "active");
    if active && library_complete(state, subscribe) {
        tracing::info!(subscribe_id = %subscribe.id, "订阅已全部入库，标记 completed，后续搜索跳过索引站");
        set_state(state, subscribe, COMPLETED);
    }
}

/// Search worker gate. Returns true when the search should be skipped.
pub(crate) fn skip_search(state: &ApiState, subscribe: &mut Subscribe) -> bool {
    if subscribe.tracking_state != COMPLETED {
        return false;
    }
    if library_complete(state, subscribe) {
        return true;
    }
    tracing::info!(subscribe_id = %subscribe.id, "已完成订阅缺失文件或范围变化，恢复 active 并重新搜索");
    set_state(state, subscribe, "active");
    // L4: the completion notice may fire again after the next completion.
    let _ = state
        .store
        .lock()
        .delete_setting(&format!("subscribe.completed:{}", subscribe.id));
    subscribe.tracking_state = "active".into();
    false
}
