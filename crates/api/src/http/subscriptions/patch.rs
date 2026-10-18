use std::str::FromStr;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{Media, Subscribe, SubscribeId, UserId};

use crate::http::{err, ok};
use crate::management::ApiState;

use super::patch_fields::apply_patch_fields;
use super::types::PatchSubscriptionInput;

pub(crate) async fn patch_subscription(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<UserId>,
    Path(id): Path<String>,
    Json(body): Json<PatchSubscriptionInput>,
) -> Response {
    let sid = match parse_subscribe_id(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let guard = state.subscribe_guard(sid);
    let _guard = guard.lock();
    if state.subscribe_is_deleting(sid) {
        return err(
            StatusCode::CONFLICT,
            "subscription.deleting",
            "订阅正在删除，无法修改",
        );
    }
    let auto_next_season = body.auto_next_season;
    let pack_fallback_after_days = body.pack_fallback_after_days;
    let (previous, subscribe, media) = match load_and_patch(&state, sid, user_id, body) {
        Ok(result) => result,
        Err(response) => return response,
    };
    if let Err(response) = persist_subscription(&state, &previous, &subscribe) {
        return response;
    }
    if let Some(days) = pack_fallback_after_days {
        crate::pack_fallback::set_after_days(&state, subscribe.id, (days > 0).then_some(days));
    }
    if let Some(auto) = auto_next_season {
        crate::completion::set_auto_next_season(&state, subscribe.id, auto);
    }
    let store = state.store.lock();
    let facts = store.load_subscribe_facts(subscribe.id).unwrap_or_default();
    let body = super::subscription_json(&store, state.catalog.as_ref(), &subscribe, &media, &facts);
    ok(body).into_response()
}

fn parse_subscribe_id(id: &str) -> Result<SubscribeId, Response> {
    SubscribeId::from_str(id).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "订阅 id 无效",
        )
    })
}

fn load_and_patch(
    state: &ApiState,
    id: SubscribeId,
    user_id: UserId,
    body: PatchSubscriptionInput,
) -> Result<(Subscribe, Subscribe, Media), Response> {
    let store = state.store.lock();
    let Some(mut subscribe) = store.get_subscribe(id).ok().flatten() else {
        return Err(err(
            StatusCode::NOT_FOUND,
            "subscription.missing",
            "订阅不存在",
        ));
    };
    if let Err(response) = super::ensure_subscribe_owner(&store, &subscribe, user_id) {
        return Err(response);
    }
    let previous = subscribe.clone();
    apply_patch_fields(&store, &mut subscribe, body)?;
    let Some(media) = store.get_media(subscribe.media_id).ok().flatten() else {
        return Err(err(
            StatusCode::NOT_FOUND,
            "subscription.missing",
            "订阅关联的影视不存在",
        ));
    };
    Ok((previous, subscribe, media))
}

fn persist_subscription(
    state: &ApiState,
    previous: &Subscribe,
    subscribe: &Subscribe,
) -> Result<(), Response> {
    // G-M1: a coverage / library change must not overlap a sibling subscription.
    if previous.coverage != subscribe.coverage || previous.library_id != subscribe.library_id {
        let overlap =
            crate::subscribe_create::overlapping_subscription(&state.store.lock(), subscribe);
        if let Ok(Some(other)) = overlap {
            return Err(err(
                StatusCode::CONFLICT,
                "subscription.overlap",
                &format!("同一媒体库中已有覆盖重叠的订阅（{other}），请合并或调整集数范围"),
            ));
        }
    }
    if let Err(error) = state.store.lock().update_subscribe(subscribe) {
        return Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ));
    }
    let subscribe_id = subscribe.id.to_string();
    if let Err(error) = super::patch_jobs::sync_jobs(
        state,
        &subscribe_id,
        subscribe.tracking_state == "paused",
        subscribe.search_interval_secs,
    ) {
        rollback_subscription(state, &subscribe_id, previous);
        tracing::error!(subscribe_id, error = %error, "订阅更新已回滚：搜索 Job 同步失败");
        return Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "subscription.job_sync_failed",
            &error.to_string(),
        ));
    }
    Ok(())
}

fn rollback_subscription(state: &ApiState, id: &str, previous: &Subscribe) {
    if let Err(error) = state.store.lock().update_subscribe(previous) {
        tracing::error!(subscribe_id = id, error = %error, "队列同步失败后回滚订阅状态失败");
    }
    super::patch_jobs::rollback_jobs(state, id, previous);
}
