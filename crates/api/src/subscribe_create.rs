use domain::{Filter, Media, Subscribe};

use crate::management::ApiState;
use crate::store::StoreError;

#[derive(Debug, thiserror::Error)]
pub(crate) enum CreateError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Job(#[from] jobs::JobError),
    /// G-M1: another subscription of this media in the same library already
    /// covers some of these units.
    #[error("同一媒体库中已有覆盖重叠的订阅 {0}")]
    Overlap(domain::SubscribeId),
}

/// G-M1: whether two coverages share at least one unit.
pub(crate) fn coverages_overlap(a: &domain::Coverage, b: &domain::Coverage) -> bool {
    use domain::Coverage;
    match (a, b) {
        (Coverage::Movie, Coverage::Movie) => true,
        (
            Coverage::Tv {
                season: sa,
                episode_from: fa,
                episode_to: ta,
            },
            Coverage::Tv {
                season: sb,
                episode_from: fb,
                episode_to: tb,
            },
        ) => {
            let end = |to: &Option<u32>| to.unwrap_or(u32::MAX);
            sa == sb && *fa <= end(tb) && *fb <= end(ta)
        }
        _ => false,
    }
}

/// G-M1: an existing subscription of `media` in the same library whose
/// coverage overlaps `subscribe` (excluding `subscribe` itself).
pub(crate) fn overlapping_subscription(
    store: &crate::Store,
    subscribe: &Subscribe,
) -> Result<Option<domain::SubscribeId>, StoreError> {
    Ok(store
        .list_all_subscribes()?
        .into_iter()
        .find(|other| {
            other.id != subscribe.id
                && other.media_id == subscribe.media_id
                && other.library_id == subscribe.library_id
                && coverages_overlap(&other.coverage, &subscribe.coverage)
        })
        .map(|other| other.id))
}

/// Persist a Subscribe and its scheduled Jobs as one logical operation.
/// The stores live in separate SQLite files, so failures need compensation.
pub(crate) fn create(
    state: &ApiState,
    incoming: Media,
    mut subscribe: Subscribe,
    filter: Option<&Filter>,
) -> Result<(Media, Subscribe), CreateError> {
    let (media, previous) = {
        let store = state.store.lock();
        let (media, previous) = store.ensure_media_with_previous(incoming)?;
        subscribe.media_id = media.id;
        match overlapping_subscription(&store, &subscribe) {
            Ok(Some(other)) => {
                rollback_store(&store, &media, previous.as_ref(), &subscribe, None);
                return Err(CreateError::Overlap(other));
            }
            Ok(None) => {}
            Err(error) => {
                rollback_store(&store, &media, previous.as_ref(), &subscribe, None);
                return Err(error.into());
            }
        }
        let inserted = (|| -> Result<(), StoreError> {
            if let Some(filter) = filter {
                store.insert_filter(filter)?;
            }
            store.insert_subscribe(&subscribe)
        })();
        if let Err(error) = inserted {
            rollback_store(&store, &media, previous.as_ref(), &subscribe, filter);
            return Err(error.into());
        }
        (media, previous)
    };

    if let Err(error) = seed_jobs(state, &media, &subscribe) {
        tracing::error!(subscribe_id = %subscribe.id, error = %error, "创建订阅任务失败");
        rollback_store(
            &state.store.lock(),
            &media,
            previous.as_ref(),
            &subscribe,
            filter,
        );
        return Err(error.into());
    }
    tracing::info!(subscribe_id = %subscribe.id, media = %media.title, "已创建新订阅");
    Ok((media, subscribe))
}

fn seed_jobs(state: &ApiState, media: &Media, subscribe: &Subscribe) -> Result<(), jobs::JobError> {
    let search_payload =
        serde_json::json!({ "subscribe_id": subscribe.id.to_string() }).to_string();
    let catalog_payload = serde_json::json!({ "media_id": media.id.to_string() }).to_string();
    let jobs = state.jobs.lock();
    let mut catalog_existed = true;
    let result = jobs.defs_for_payload(&catalog_payload).and_then(|defs| {
        catalog_existed = !defs.is_empty();
        crate::jobs_api::seed_subscribe_search_every(
            &jobs,
            &subscribe.id.to_string(),
            subscribe.search_interval_secs,
            Some(&media.title),
        )
        .and_then(|()| {
            crate::jobs_api::seed_catalog_refresh(&jobs, &media.id.to_string(), Some(&media.title))
        })
    });
    if result.is_err() {
        if let Err(error) = jobs.delete_defs_for_payload(&search_payload) {
            tracing::error!(subscribe_id = %subscribe.id, error = %error, "回滚搜索任务失败");
        }
        if !catalog_existed {
            if let Err(error) = jobs.delete_defs_for_payload(&catalog_payload) {
                tracing::error!(media_id = %media.id, error = %error, "回滚目录刷新任务失败");
            }
        }
    }
    result
}

fn rollback_store(
    store: &crate::Store,
    media: &Media,
    previous: Option<&Media>,
    subscribe: &Subscribe,
    filter: Option<&Filter>,
) {
    if let Err(error) = store.delete_subscribe(subscribe.id) {
        tracing::error!(subscribe_id = %subscribe.id, error = %error, "回滚订阅失败");
    }
    if let Some(filter) = filter {
        if let Err(error) = store.delete_filter(filter.id) {
            tracing::error!(filter_id = %filter.id, error = %error, "回滚规则组失败");
        }
    }
    match store.undo_ensured_media(media, previous) {
        Ok(true) => {}
        Ok(false) => {
            // `false` 有两种含义，都不能盲目回滚：
            // (a) 这行仍被 Subscribe/ledger 引用；(b) 身份已被并发请求改写（CAS 拒绝）。
            tracing::warn!(
                media_id = %media.id,
                "回滚 Media 被拒绝（仍被引用或身份已被并发改写），保留当前内容"
            );
        }
        Err(error) => {
            tracing::error!(media_id = %media.id, error = %error, "回滚 Media 失败");
        }
    }
}

#[cfg(test)]
mod overlap_tests {
    use domain::Coverage;

    #[test]
    fn ranges_overlap() {
        let tv = |from, to| Coverage::Tv {
            season: 1,
            episode_from: from,
            episode_to: to,
        };
        assert!(super::coverages_overlap(&tv(1, Some(5)), &tv(5, Some(8))));
        assert!(!super::coverages_overlap(&tv(1, Some(5)), &tv(6, Some(8))));
        assert!(super::coverages_overlap(&tv(1, None), &tv(9, Some(9))));
        assert!(super::coverages_overlap(&Coverage::Movie, &Coverage::Movie));
    }
}
