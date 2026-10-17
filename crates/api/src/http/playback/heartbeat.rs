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
    kind: domain::MediaKind,
    ledger_id: String,
    season: i32,
    episode: i32,
    duration_ms: Option<i64>,
    now: i64,
    now_ms: i64,
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
    if let Some(rate_val) = body.get("rate") {
        if let Some(r) = rate_val.as_f64() {
            if !r.is_finite() || r < 0.25 || r > 4.0 {
                return err(
                    StatusCode::BAD_REQUEST,
                    "playback.invalid_rate",
                    "播放倍率必须在 0.25 到 4.0 之间",
                );
            }
        }
    }
    let play_session_id = body["play_session_id"].as_str().map(str::to_string);
    let event_sequence = body["event_sequence"].as_i64();
    if let Some(ref sid) = play_session_id {
        if sid.len() > 128 {
            return err(StatusCode::BAD_REQUEST, "playback.invalid", "play_session_id 超长");
        }
    }
    if let Some(seq) = event_sequence {
        if seq < 0 {
            return err(StatusCode::BAD_REQUEST, "playback.invalid", "event_sequence 必须非负");
        }
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
    let now_ms = state.playback_clock.now_ms();
    let heartbeat = Heartbeat {
        body: &body,
        user_id,
        media_id: media.id,
        kind: media.kind,
        ledger_id: row.id.to_string(),
        season,
        episode,
        duration_ms,
        now: now_ms / 1000,
        now_ms,
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
    let rate = body["rate"].as_f64().unwrap_or(1.0);
    let now_ms = heartbeat.now_ms;
    let delta = previous
        .map(|prev| crate::playback_events::watched_increment(prev, position_ms, paused, false, now_ms, rate))
        .unwrap_or(0);
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
        watched_ms: previous.map(|s| s.watched_ms).unwrap_or(0).saturating_add(delta),
        rate_bps: 0,
        bytes_sent: 0,
        connections: 1,
        admin_ended: previous.is_some_and(|s| s.admin_ended),
        started_at: previous.map(|s| s.started_at).unwrap_or(heartbeat.now),
        last_report_at: heartbeat.now,
        ledger_id: Some(heartbeat.ledger_id.clone()),
        play_session_id: previous.and_then(|s| s.play_session_id.clone()),
        last_report_at_ms: heartbeat.now_ms,
        last_event_sequence: previous.and_then(|s| s.last_event_sequence),
    }
}

fn heartbeat_matches_row(heartbeat: &Heartbeat<'_>, row: &domain::LedgerRow) -> bool {
    row.media_id == heartbeat.media_id
        && row.season.map(|value| value as i32) == (heartbeat.season != UNIT_WHOLE).then_some(heartbeat.season)
        && row.episode.map(|value| value as i32) == (heartbeat.episode != UNIT_WHOLE).then_some(heartbeat.episode)
}

fn persist_heartbeat(
    store: &crate::Store,
    heartbeat: &Heartbeat<'_>,
    device: &str,
) -> Result<(UnitState, bool), StoreError> {
    // The row may have been canonically rebound after initial selection. Never
    // persist an event against the stale Media/unit captured by that selection.
    if let Some(row) = store.get_ledger(&heartbeat.ledger_id)? {
        if !heartbeat_matches_row(heartbeat, &row) {
            tracing::warn!(ledger_id = %heartbeat.ledger_id, media_id = %heartbeat.media_id,
                "Ignoring stale Playback heartbeat after ledger identity changed");
            return Ok((
                store.unit_state(heartbeat.user_id, heartbeat.media_id, heartbeat.season, heartbeat.episode)?
                    .unwrap_or_default(),
                true,
            ));
        }
        if let Some(owner) = store.library_for_path_strict(std::path::Path::new(&row.path), heartbeat.kind)? {
            if !store.library_accepts_work(&owner.id)? {
                return Ok((
                    store.unit_state(heartbeat.user_id, heartbeat.media_id, heartbeat.season, heartbeat.episode)?
                        .unwrap_or_default(),
                    true,
                ));
            }
        }
    } else {
        return Ok((
            store.unit_state(heartbeat.user_id, heartbeat.media_id, heartbeat.season, heartbeat.episode)?
                .unwrap_or_default(),
            true,
        ));
    }

    let event = heartbeat.body["event"].as_str().unwrap_or("progress");
    let previous = store.get_session(heartbeat.user_id, device)?;
    let play_session_id = heartbeat.body["play_session_id"].as_str().map(str::to_string);
    let event_sequence = heartbeat.body["event_sequence"].as_i64();

    let binding = if let Some(ref sid) = play_session_id {
        store.play_session_binding(heartbeat.user_id, device, sid)?
    } else {
        None
    };

    let identity = crate::playback_events::PlaybackEventIdentity {
        play_session_id: play_session_id.clone(),
        event_sequence,
    };

    let event_kind = match event {
        "start" => crate::playback_events::PlaybackEventKind::Start,
        "stop" => crate::playback_events::PlaybackEventKind::Stop,
        _ => crate::playback_events::PlaybackEventKind::Progress,
    };

    let ledger_id = match heartbeat.ledger_id.parse::<domain::LedgerId>() {
        Ok(lid) => lid,
        Err(_) => return Ok((store.unit_state(heartbeat.user_id, heartbeat.media_id, heartbeat.season, heartbeat.episode)?.unwrap_or_default(), true)),
    };

    let was_admin_ended = if previous.is_none() && event != "start" {
        store
            .device_was_admin_ended(heartbeat.user_id, device, heartbeat.now, 10)
            .unwrap_or(false)
    } else {
        false
    };

    let decision = if was_admin_ended {
        crate::playback_events::EventDecision::Ignore
    } else {
        crate::playback_events::classify_event(
            previous.as_ref(),
            binding.as_ref(),
            &identity,
            ledger_id,
            event_kind,
        )
    };

    match decision {
        crate::playback_events::EventDecision::Ignore => {
            let admin_ended = previous.as_ref().is_some_and(|p| p.admin_ended) || was_admin_ended;
            return Ok((
                store
                    .unit_state(heartbeat.user_id, heartbeat.media_id, heartbeat.season, heartbeat.episode)?
                    .unwrap_or_default(),
                admin_ended,
            ));
        }
        crate::playback_events::EventDecision::Start => {
            if let Some(ref sid) = play_session_id {
                store.activate_play_session(heartbeat.user_id, device, sid)?;
            }
        }
        crate::playback_events::EventDecision::Stop => {
            if let Some(ref sid) = play_session_id {
                store.close_play_session_id(heartbeat.user_id, device, sid)?;
            }
        }
        crate::playback_events::EventDecision::Update => {}
    }

    let is_new_start = decision == crate::playback_events::EventDecision::Start;
    let unit = update_unit(store, heartbeat, if is_new_start && event == "start" { "start" } else { event })?;
    let next = next_session(
        heartbeat,
        &unit,
        device,
        previous.as_ref(),
        is_new_start,
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
        let mut to_save = next;
        if play_session_id.is_some() {
            to_save.play_session_id = play_session_id;
            to_save.last_event_sequence = event_sequence;
        }
        store.upsert_session(&to_save)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_heartbeat_after_canonical_rebind_does_not_write_old_media() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::Store::open(temp.path()).unwrap();
        let library = store.list_libraries().unwrap().into_iter()
            .find(|library| library.kind == domain::MediaKind::Movie).unwrap();
        let provisional = domain::Media { id: domain::MediaId::new(), kind: domain::MediaKind::Movie,
            title: "provisional".into(), year: None, original_title: None, aliases: Vec::new(), tmdb_id: None,
            douban_id: None, tvdb_id: None, bangumi_id: None, anilist_id: None };
        let canonical = domain::Media { id: domain::MediaId::new(), kind: domain::MediaKind::Movie,
            title: "canonical".into(), year: None, original_title: None, aliases: Vec::new(), tmdb_id: Some("123".into()),
            douban_id: None, tvdb_id: None, bangumi_id: None, anilist_id: None };
        store.insert_media(&provisional).unwrap();
        store.insert_media(&canonical).unwrap();
        let ledger = domain::LedgerRow { id: domain::LedgerId::new(), media_id: provisional.id,
            path: library.root_paths[0].join("movie.mkv").display().to_string(), season: None, episode: None,
            resolution: None, codec: None, hdr: None, quality_source: domain::QualitySource::Probe,
            confidence: domain::Confidence::High, filter_score: None };
        store.insert_ledger(&ledger).unwrap();
        store.rebind_scanned_ledger(ledger.id, canonical.id).unwrap();
        let user_id = domain::UserId::new();
        let body = serde_json::json!({"event":"start", "position_ms":500, "device_id":"race"});
        let heartbeat = Heartbeat { body: &body, user_id, media_id: provisional.id,
            kind: domain::MediaKind::Movie, ledger_id: ledger.id.to_string(),
            season: UNIT_WHOLE, episode: UNIT_WHOLE, duration_ms: Some(1000), now: 10, now_ms: 10_000 };

        let (_, ended) = persist_heartbeat(&store, &heartbeat, "race").unwrap();

        assert!(ended);
        assert!(store.unit_state(user_id, provisional.id, UNIT_WHOLE, UNIT_WHOLE).unwrap().is_none());
        assert!(store.get_session(user_id, "race").unwrap().is_none());
    }
}
