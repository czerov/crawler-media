//! Resume, watched and favorite state endpoints.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::resolve_media_id;
use crate::http::{err, ok};
use crate::management::ApiState;
use crate::store::{UNIT_WHOLE, UnitRow};

/// GET /playback/resume — per-unit resume state.
pub(crate) async fn resume(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(media_id) = query.get("media_item_id").and_then(|v| resolve_media_id(v)) else {
        return err(
            StatusCode::BAD_REQUEST,
            "playback.invalid",
            "media_item_id 无效",
        );
    };
    let season = query
        .get("season_number")
        .and_then(|v| v.parse().ok())
        .unwrap_or(UNIT_WHOLE);
    let episode = query
        .get("episode_number")
        .and_then(|v| v.parse().ok())
        .unwrap_or(UNIT_WHOLE);
    let store = state.store.lock();
    let (season, episode) = super::selection::watch_unit(&store, media_id, season, episode);
    ok(super::movie_compatible_unit(
        &store, user_id, media_id, season, episode,
    ))
    .into_response()
}

/// GET /playback/marks — played/favorite + unplayed_count for a target.
pub(crate) async fn marks(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(media_id) = query.get("media_item_id").and_then(|v| resolve_media_id(v)) else {
        return err(
            StatusCode::BAD_REQUEST,
            "playback.invalid",
            "media_item_id 无效",
        );
    };
    let season = query
        .get("season_number")
        .and_then(|v| v.parse().ok())
        .unwrap_or(UNIT_WHOLE);
    let episode = query
        .get("episode_number")
        .and_then(|v| v.parse().ok())
        .unwrap_or(UNIT_WHOLE);
    let store = state.store.lock();
    let (season, episode) = super::selection::watch_unit(&store, media_id, season, episode);
    let rows = store.unit_rows(user_id, media_id).unwrap_or_default();
    let watch = super::movie_compatible_unit(&store, user_id, media_id, season, episode);
    ok(json!({
        "played": watch["played"].as_bool().unwrap_or(false),
        "is_favorite": favorite_for(&store, user_id, media_id, season, episode, &rows),
        "unplayed_count": unplayed_count(&rows, season, episode),
    }))
    .into_response()
}

fn favorite_for(
    store: &crate::Store,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
    rows: &[UnitRow],
) -> bool {
    if season == UNIT_WHOLE && episode == UNIT_WHOLE {
        if let Some(true) = explicit_favorite(store, user_id, media_id, season, episode) {
            return true;
        }
        return rows.iter().any(|row| row.favorite);
    }
    if let Some(favorite) = explicit_favorite(store, user_id, media_id, season, episode) {
        return favorite;
    }
    inherited_favorite(store, user_id, media_id, season, episode)
}

fn explicit_favorite(
    store: &crate::Store,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> Option<bool> {
    store
        .unit_state(user_id, media_id, season, episode)
        .ok()
        .flatten()
        .map(|unit| unit.favorite)
}

fn inherited_favorite(
    store: &crate::Store,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> bool {
    if explicit_favorite(store, user_id, media_id, UNIT_WHOLE, UNIT_WHOLE) == Some(true) {
        return true;
    }
    season >= 0
        && episode >= 0
        && explicit_favorite(store, user_id, media_id, season, UNIT_WHOLE) == Some(true)
}

fn unplayed_count(rows: &[UnitRow], season: i32, episode: i32) -> Option<i64> {
    if episode != UNIT_WHOLE {
        return None;
    }
    let count = rows
        .iter()
        .filter(|row| {
            row.episode >= 0 && (season == UNIT_WHOLE || row.season == season) && !row.played
        })
        .count();
    (count > 0).then_some(count as i64)
}

/// POST /playback/marks — set played (cascade) / favorite (target level).
pub(crate) async fn set_marks(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Json(body): Json<Value>,
) -> Response {
    let Some(media_id) = body["media_item_id"]
        .as_str()
        .and_then(|v| resolve_media_id(v))
    else {
        return err(
            StatusCode::BAD_REQUEST,
            "playback.invalid",
            "media_item_id 无效",
        );
    };
    let season = body["season_number"].as_i64().unwrap_or(-1) as i32;
    let episode = body["episode_number"].as_i64().unwrap_or(-1) as i32;
    let store = state.store.lock();
    let (season, episode) = super::selection::watch_unit(&store, media_id, season, episode);
    let played = body["played"].as_bool();
    let favorite = body["favorite"].as_bool();
    // `playback_clock` 是毫秒时钟；`playback_units.updated_at` 存 unix 秒。
    let now = state.playback_clock.now_ms() / 1000;
    if let Err(error) =
        store.set_unit_marks(user_id, media_id, season, episode, played, favorite, now)
    {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    let unit = super::movie_compatible_unit(&store, user_id, media_id, season, episode);
    ok(json!({
        "played": unit["played"].as_bool().unwrap_or(false),
        "is_favorite": unit["is_favorite"].as_bool().unwrap_or(false),
        "unplayed_count": null,
    }))
    .into_response()
}
