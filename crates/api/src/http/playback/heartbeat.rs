//! Device-scoped Playback heartbeats.
use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::Value;

use super::{movie_compatible_state, row_for, watch_state_json};
use crate::store::{SessionRow, StoreError, UNIT_WHOLE, UnitState};
use crate::{
    http::{err, ok},
    job_loop::unix_now,
    management::ApiState,
};

struct Heartbeat<'a> {
    body: &'a Value,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    ledger_id: String,
    season: i32,
    episode: i32,
    duration_ms: Option<i64>,
    now: i64,
}

pub(crate) async fn progress(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Json(body): Json<Value>,
) -> Response {
    let device_id = body["device_id"].as_str().unwrap_or_default();
    if device_id.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "playback.invalid",
            "缺少 device_id",
        );
    }
    let (row, media, season, episode, duration_ms) = {
        let store = state.store.lock();
        match store.is_device_revoked(user_id, device_id) {
            Ok(true) => return err(StatusCode::FORBIDDEN, "playback.revoked", "该设备已被注销"),
            Ok(false) => {}
            Err(error) => return persistence_error(&error, user_id, device_id),
        }
        let Some((row, media, season, episode)) = row_for(&store, &body, Some(user_id)) else {
            return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
        };
        let duration = match unit_duration(&store, user_id, &row, season, episode) {
            Ok(duration) => body["duration_ms"].as_i64().filter(|d| *d > 0).or(duration),
            Err(error) => return persistence_error(&error, user_id, device_id),
        };
        (row, media, season, episode, duration)
    };
    if duration_ms.is_none() {
        let _ = crate::http::library::ensure_probe_enqueued(&state, &row);
    }
    let heartbeat = Heartbeat {
        body: &body,
        user_id,
        media_id: media.id,
        ledger_id: row.id.to_string(),
        season,
        episode,
        duration_ms,
        now: unix_now(),
    };
    apply_heartbeat(&state.store.lock(), &heartbeat, device_id)
}

fn persistence_error(error: &StoreError, user_id: domain::UserId, device_id: &str) -> Response {
    tracing::error!(%error, %user_id, device_id, "Playback persistence failed");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        "保存播放状态失败",
    )
}

fn unit_duration(
    store: &crate::Store,
    user: domain::UserId,
    row: &domain::LedgerRow,
    season: i32,
    episode: i32,
) -> Result<Option<i64>, StoreError> {
    if let Some(duration) = movie_compatible_state(store, user, row.media_id, season, episode)?
        .and_then(|unit| unit.duration_ms)
    {
        return Ok(Some(duration));
    }
    Ok(store
        .get_file_meta(&row.id.to_string())?
        .and_then(|tracks| tracks.video.and_then(|video| video.duration_secs))
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .map(|seconds| (seconds * 1000.0) as i64))
}

fn update_unit(
    store: &crate::Store,
    heartbeat: &Heartbeat<'_>,
    event: &str,
) -> Result<UnitState, StoreError> {
    if (heartbeat.season, heartbeat.episode) == (UNIT_WHOLE, UNIT_WHOLE) {
        store.copy_legacy_movie_unit_if_missing(heartbeat.user_id, heartbeat.media_id)?;
    }
    let existing = store.unit_state(
        heartbeat.user_id,
        heartbeat.media_id,
        heartbeat.season,
        heartbeat.episode,
    )?;
    // A start with no position preserves the saved resume point.
    let position = heartbeat.body["position_ms"]
        .as_i64()
        .or_else(|| {
            (event == "start")
                .then(|| existing.as_ref().map(|u| u.position_ms))
                .flatten()
        })
        .unwrap_or(0)
        .max(0);
    let duration = heartbeat
        .duration_ms
        .or(existing.as_ref().and_then(|u| u.duration_ms));
    let completed = UnitState::playback_completed(position, duration);
    store.upsert_unit(
        heartbeat.user_id,
        heartbeat.media_id,
        heartbeat.season,
        heartbeat.episode,
        position,
        completed.then_some(true),
        None,
        duration,
        heartbeat.body["audio_track"].as_str(),
        heartbeat.body["subtitle_track"].as_str(),
        event == "start",
        heartbeat.now,
    )
}

fn next_session(
    heartbeat: &Heartbeat<'_>,
    unit: &UnitState,
    device: &str,
    previous: Option<&SessionRow>,
    start: bool,
) -> SessionRow {
    let body = heartbeat.body;
    let position_ms = unit.position_ms;
    let paused = body["paused"].as_bool().unwrap_or(false);
    let previous = previous.filter(|_| !start);
    let delta = (position_ms - previous.map(|s| s.position_ms).unwrap_or(position_ms)).max(0);
    SessionRow {
        device_id: device.to_string(),
        user_id: heartbeat.user_id,
        media_id: heartbeat.media_id,
        season: (heartbeat.season != UNIT_WHOLE).then_some(heartbeat.season),
        episode: (heartbeat.episode != UNIT_WHOLE).then_some(heartbeat.episode),
        client: body["client"].as_str().map(str::to_string),
        device_name: body["device_name"].as_str().map(str::to_string),
        client_version: body["client_version"].as_str().map(str::to_string),
        play_method: "local".into(),
        position_ms,
        start_position_ms: previous.map(|s| s.start_position_ms).unwrap_or(position_ms),
        duration_ms: unit.duration_ms,
        paused,
        watched_ms: previous.map(|s| s.watched_ms).unwrap_or(0) + if paused { 0 } else { delta },
        rate_bps: 0,
        bytes_sent: 0,
        connections: 1,
        admin_ended: previous.is_some_and(|s| s.admin_ended),
        started_at: previous.map(|s| s.started_at).unwrap_or(heartbeat.now),
        last_report_at: heartbeat.now,
        ledger_id: Some(heartbeat.ledger_id.clone()),
    }
}

fn persist_heartbeat(
    store: &crate::Store,
    heartbeat: &Heartbeat<'_>,
    device: &str,
) -> Result<(UnitState, bool), StoreError> {
    let event = heartbeat.body["event"].as_str().unwrap_or("progress");
    let previous = store.get_session(heartbeat.user_id, device)?;
    let unit = update_unit(store, heartbeat, event)?;
    if previous.is_none() && event != "start" {
        // An ended session must not be implicitly resurrected by a heartbeat.
        return Ok((unit, true));
    }
    let next = next_session(
        heartbeat,
        &unit,
        device,
        previous.as_ref(),
        event == "start",
    );
    let admin_ended = next.admin_ended && event != "stop";
    if event == "stop" {
        if let Some(mut closing) = previous {
            closing.watched_ms = next.watched_ms;
            closing.position_ms = next.position_ms;
            closing.duration_ms = next.duration_ms;
            store.upsert_session(&closing)?;
        }
    } else if !admin_ended {
        store.upsert_session(&next)?;
    }
    if event == "stop" || admin_ended {
        store.close_session(heartbeat.user_id, device, heartbeat.now)?;
    }
    Ok((unit, admin_ended))
}

fn apply_heartbeat(store: &crate::Store, heartbeat: &Heartbeat<'_>, device: &str) -> Response {
    match store.playback_write(|store| persist_heartbeat(store, heartbeat, device)) {
        Ok((unit, ended)) => ok(watch_state_json(&unit, ended)).into_response(),
        Err(error) => persistence_error(&error, heartbeat.user_id, device),
    }
}
