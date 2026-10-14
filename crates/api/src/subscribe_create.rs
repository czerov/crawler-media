use domain::{Filter, Media, Subscribe};

use crate::management::ApiState;
use crate::store::StoreError;

#[derive(Debug, thiserror::Error)]
pub(crate) enum CreateError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Job(#[from] jobs::JobError),
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
    if let Err(error) = store.undo_ensured_media(media, previous) {
        tracing::error!(media_id = %media.id, error = %error, "回滚 Media 失败");
    }
}
