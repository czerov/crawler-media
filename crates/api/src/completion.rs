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
    units_on_disk(state, subscribe)
}

/// Every unit of the (bounded) coverage has a fact whose file exists.
/// Unlike `library_complete`, wash-cut subscriptions are not excluded.
pub(crate) fn units_on_disk(state: &ApiState, subscribe: &Subscribe) -> bool {
    // G-L1: only facts inside the subscription's target library count.
    let kind = match subscribe.coverage {
        Coverage::Movie => domain::MediaKind::Movie,
        Coverage::Tv { .. } => domain::MediaKind::Tv,
    };
    let Ok(facts) = state
        .store
        .lock()
        .load_library_subscribe_facts(subscribe, kind)
    else {
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
    if library_complete(state, subscribe) {
        create_next_season(state, subscribe);
    }
}

fn auto_next_key(id: domain::SubscribeId) -> String {
    format!("subscribe.auto_next_season:{id}")
}

fn next_created_key(id: domain::SubscribeId) -> String {
    format!("subscribe.next_season_created:{id}")
}

/// G-M3: opt-in per subscription (stored as a setting, default off).
pub(crate) fn set_auto_next_season(state: &ApiState, id: domain::SubscribeId, on: bool) {
    let store = state.store.lock();
    let result = if on {
        store.put_setting(&auto_next_key(id), "1")
    } else {
        store.delete_setting(&auto_next_key(id)).map(|_| ())
    };
    if let Err(error) = result {
        tracing::warn!(subscribe_id = %id, %error, "保存 auto_next_season 失败");
    }
}

/// G-M3: once a single-season subscription completes and the catalog lists
/// season S+1, create one open subscription for it (copying filter, library,
/// downloader and wash settings). Idempotent. Call without the store lock.
fn create_next_season(state: &ApiState, subscribe: &Subscribe) {
    let Coverage::Tv { season, .. } = subscribe.coverage else {
        return;
    };
    let media = {
        let store = state.store.lock();
        let enabled = store
            .get_setting(&auto_next_key(subscribe.id))
            .ok()
            .flatten()
            .is_some();
        let done = store
            .get_setting(&next_created_key(subscribe.id))
            .ok()
            .flatten()
            .is_some();
        if !enabled || done {
            return;
        }
        match store.get_media(subscribe.media_id) {
            Ok(Some(media)) => media,
            _ => return,
        }
    };
    let Some(tmdb_id) = media.tmdb_id.as_deref() else {
        return;
    };
    let next = season + 1;
    match state.catalog.season_episodes(tmdb_id, next) {
        Ok(episodes) if !episodes.is_empty() => {}
        _ => return,
    }
    let mut follow = subscribe.clone();
    follow.id = domain::SubscribeId::new();
    follow.coverage = Coverage::Tv {
        season: next,
        episode_from: 1,
        episode_to: None,
    };
    follow.tracking_state = "active".into();
    match crate::subscribe_create::create(state, media.clone(), follow, None) {
        Ok((media, created)) => {
            let store = state.store.lock();
            let _ = store.put_setting(&next_created_key(subscribe.id), &created.id.to_string());
            let _ = store.put_setting(&auto_next_key(created.id), "1");
            drop(store);
            tracing::info!(subscribe_id = %subscribe.id, next_subscribe = %created.id, season = next, "已自动创建下一季订阅");
            crate::worker::notify(
                state,
                "已自动订阅下一季",
                &format!("「{}」第 {next} 季已创建订阅", media.title),
            );
        }
        Err(crate::subscribe_create::CreateError::Overlap(other)) => {
            // A subscription for that season already exists: nothing to do.
            let _ = state
                .store
                .lock()
                .put_setting(&next_created_key(subscribe.id), &other.to_string());
        }
        Err(error) => {
            tracing::warn!(subscribe_id = %subscribe.id, %error, "自动创建下一季订阅失败");
        }
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
