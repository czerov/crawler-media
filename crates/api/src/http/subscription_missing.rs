//! Manual search for still-missing Subscribe coverage.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::http::{err, ok};
use crate::management::ApiState;

type Unit = (Option<u32>, Option<u32>);

pub(crate) async fn run_missing_search(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
) -> Response {
    let (initial, _) = match super::subscriptions::resolve_subscribe(&state, &id, user_id) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let guard = state.subscribe_guard(initial.id);
    let _guard = guard.lock();
    let (subscribe, media) = match super::subscriptions::resolve_subscribe(&state, &id, user_id) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    if subscribe.tracking_state == "paused" {
        return err(
            StatusCode::CONFLICT,
            "subscription.paused",
            "订阅已暂停，无法搜索缺失资源",
        );
    }
    queue_missing_search(&state, &subscribe, &media)
}

fn queue_missing_search(
    state: &ApiState,
    subscribe: &domain::Subscribe,
    media: &domain::Media,
) -> Response {
    let missing = match missing_units(state, subscribe, media) {
        Ok(units) => units,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    if missing.is_empty() {
        return err(
            StatusCode::CONFLICT,
            "subscription.no_missing_resources",
            "订阅当前没有可搜索的缺失资源",
        );
    }
    if let Err(error) = crate::jobs_api::seed_subscribe_search(
        &state.jobs.lock(),
        &subscribe.id.to_string(),
        Some(&media.title),
    ) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "job.seed_failed",
            &error.to_string(),
        );
    }
    let reset_count = match state
        .store
        .lock()
        .reset_wanted_search_cooldowns(subscribe.id, &missing)
    {
        Ok(count) => count,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    tracing::info!(
        subscribe_id = %subscribe.id,
        missing_units = missing.len(),
        reset_count,
        "已为缺失资源排入立即搜索"
    );
    ok(json!({ "queued": true, "reset_count": reset_count })).into_response()
}

fn missing_units(
    state: &ApiState,
    subscribe: &domain::Subscribe,
    media: &domain::Media,
) -> Result<Vec<Unit>, crate::store::StoreError> {
    let cap = crate::air_gate::aired_cap(state, subscribe, media);
    let store = state.store.lock();
    let facts = store.load_subscribe_facts(subscribe.id)?;
    let pending = store.load_pending(subscribe.id)?;
    let history = store.load_wanted_history(subscribe.id)?;
    Ok(crate::air_gate::cap_units(
        crate::open_coverage::units(
            subscribe,
            media,
            &facts,
            &pending,
            &history,
            std::iter::empty(),
        ),
        cap,
    )
    .into_iter()
    .filter(|(season, episode)| {
        facts.get(*season, *episode).is_none()
            && !history
                .get(&(*season, *episode))
                .is_some_and(|row| row.grabbed_at.is_some())
            && !pending.iter().any(|(_, item)| {
                super::subscription_depth::release_covers_unit(
                    &item.torrent.title,
                    *season,
                    *episode,
                )
            })
    })
    .collect())
}
