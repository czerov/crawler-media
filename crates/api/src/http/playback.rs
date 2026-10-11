//! Playback views: player-facing decide/session/progress/metrics/policy,
//! resume/marks, and item/episode info. Live activity, history/stats and
//! up-next/favorites live in sibling modules; the Jellyfin-compatible routes
//! stay in `crates/api/src/jellyfin.rs`.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::str::FromStr;

use crate::http::{err, ok};
use crate::job_loop::unix_now;
use crate::management::ApiState;
use crate::store::{UNIT_WHOLE, UnitState};

mod chapter_cache;
pub(crate) mod episodes;
mod heartbeat;
mod selection;
mod session;
pub(crate) use heartbeat::progress;
use selection::{row_for, start_position};
pub(crate) use session::start_session;
mod watch_state;
pub(crate) use chapter_cache::cached_chapters;
pub(crate) use watch_state::{marks, resume, set_marks};

const SESSION_TIMEOUT_SECS: i64 = 300;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn resolve_media_id(raw: &str) -> Option<domain::MediaId> {
    domain::MediaId::from_str(raw).ok()
}

pub(super) fn movie_compatible_unit(
    store: &crate::Store,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> Value {
    match movie_compatible_state(store, user_id, media_id, season, episode) {
        Ok(Some(unit)) => unit_json(&unit),
        Ok(None) => unit_for_unit(store, user_id, media_id, season, episode),
        Err(error) => {
            tracing::error!(%error, %user_id, %media_id, season, episode, "读取播放进度失败");
            unit_for_unit(store, user_id, media_id, season, episode)
        }
    }
}

pub(crate) fn movie_compatible_state(
    store: &crate::Store,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> Result<Option<crate::store::UnitState>, crate::store::StoreError> {
    let (season, episode) = selection::watch_unit(store, media_id, season, episode);
    if let Some(unit) = store.unit_state(user_id, media_id, season, episode)? {
        return Ok(Some(unit));
    }
    if !is_movie_media(store, media_id)? || (season, episode) != (UNIT_WHOLE, UNIT_WHOLE) {
        return Ok(None);
    }
    store.unit_state(user_id, media_id, 0, 0)
}

fn is_movie_media(
    store: &crate::Store,
    media_id: domain::MediaId,
) -> Result<bool, crate::store::StoreError> {
    Ok(store
        .get_media(media_id)?
        .is_some_and(|media| media.kind != domain::MediaKind::Tv))
}

/// Library attribution for an item: the library whose roots contain an owned
/// file, falling back to the kind's default library.
pub(crate) fn library_id_for(
    store: &crate::Store,
    media: &domain::Media,
    rows: &[&domain::LedgerRow],
) -> Option<String> {
    let mut ids = rows
        .iter()
        .filter_map(|row| crate::http::library::library_for_row(store, row, media))
        .map(|library| library.id);
    let first = ids.next()?;
    ids.all(|id| id == first).then_some(first)
}

fn visible_rows_for_media<'a>(
    store: &crate::Store,
    media: &domain::Media,
    rows: &'a [domain::LedgerRow],
    user_id: Option<domain::UserId>,
) -> Vec<&'a domain::LedgerRow> {
    rows.iter()
        .filter(|row| row.media_id == media.id)
        .filter(|row| crate::http::library::row_visible_to_user(store, row, media, user_id))
        .collect()
}

/// The activity view's media target block for a session/log row.
pub(crate) fn media_target_json(
    store: &crate::Store,
    media_id: domain::MediaId,
    season: Option<i32>,
    episode: Option<i32>,
) -> Value {
    let Some(media) = store.get_media(media_id).ok().flatten() else {
        return json!({ "media_item_id": media_id.to_string(), "title": "", "kind": "movie" });
    };
    let all_rows = store.list_ledger().unwrap_or_default();
    let rows: Vec<&domain::LedgerRow> = all_rows
        .iter()
        .filter(|row| row.media_id == media_id)
        .collect();
    let library_id = library_id_for(store, &media, &rows);
    let poster_url = crate::http::library::preferred_row(&rows)
        .and_then(crate::http::library::poster_path)
        .map(|_| {
            let row = crate::http::library::preferred_row(&rows).expect("just checked");
            crate::http::library::artwork_url("posters", row.id)
        });
    json!({
        "media_item_id": media.id.to_string(),
        "library_id": library_id,
        "browsable": true,
        "kind": media.kind.as_str(),
        "title": media.title,
        "year": media.year,
        "poster_url": poster_url,
        "season_number": season.unwrap_or(0),
        "episode_number": episode.unwrap_or(0),
        "episode_title": null,
    })
}

pub(crate) fn unit_json(unit: &UnitState) -> Value {
    json!({
        "position_ms": unit.position_ms,
        "played": unit.played,
        "play_count": unit.play_count,
        "duration_ms": unit.duration_ms,
        "audio_track": unit.audio_track,
        "subtitle_track": unit.subtitle_track,
        "is_favorite": unit.favorite,
    })
}

/// The watch-state response the player merges into its timeline.
pub(crate) fn watch_state_json(unit: &UnitState, ended_by_admin: bool) -> Value {
    let mut value = unit_json(unit);
    value["ended_by_admin"] = json!(ended_by_admin);
    value
}

// ---------------------------------------------------------------------------
// Item info + decide/session
// ---------------------------------------------------------------------------

pub(crate) async fn item(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let store = state.store.lock();
    let Some(media_id) = resolve_media_id(&id) else {
        return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
    };
    let Some(media) = store.get_media(media_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
    };
    let all_rows = store.list_ledger().unwrap_or_default();
    let rows = visible_rows_for_media(&store, &media, &all_rows, user_id);
    if rows.is_empty() {
        return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
    }
    let library_id = library_id_for(&store, &media, &rows).unwrap_or_default();
    let poster_url = crate::http::library::preferred_row(&rows)
        .and_then(|row| crate::http::library::poster_path(row).map(|_| row))
        .map(|row| crate::http::library::artwork_url("posters", row.id));
    ok(json!({
        "media_item_id": media.id.to_string(),
        "library_id": library_id,
        "kind": media.kind.as_str(),
        "title": media.title,
        "year": media.year,
        "poster_url": poster_url,
    }))
    .into_response()
}

pub(crate) async fn episodes(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let season_number = match query.get("season_number") {
        Some(s) => match s.parse::<u32>() {
            Ok(num) => Some(num),
            Err(_) => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "playback.invalid",
                    "season_number 无效",
                );
            }
        },
        None => None,
    };
    let store = state.store.lock();
    let Some(media_id) = resolve_media_id(&id) else {
        return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
    };
    let Some(media) = store.get_media(media_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
    };
    let all_rows = store.list_ledger().unwrap_or_default();
    let visible_rows = visible_rows_for_media(&store, &media, &all_rows, user_id);
    if visible_rows.is_empty() {
        return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
    }
    let season_metas = std::collections::HashMap::new();
    let visible_cloned: Vec<domain::LedgerRow> = visible_rows.into_iter().cloned().collect();
    let episode_rows = episodes::aggregate_visible_episodes(
        &store,
        &media,
        &visible_cloned,
        season_number,
        user_id,
        &season_metas,
    );
    let rows = episodes::format_episodes_json(&episode_rows, &id);
    ok(json!({ "season_number": season_number, "episodes": rows })).into_response()
}

fn direct_decision(
    row: &domain::LedgerRow,
    body: &Value,
    tracks: Option<&library::Tracks>,
) -> Value {
    let failed_tiers = body["failed_tiers"].as_array();
    let tier_0_failed = failed_tiers.is_some_and(|arr| arr.iter().any(|v| v.as_i64() == Some(0)));
    if tier_0_failed {
        return json!({
            "outcome": "rejected",
            "tier": null,
            "file_id": null,
            "container": null,
            "video": null,
            "audio": null,
            "audio_tracks": [],
            "subtitles": [],
            "degraded_from": null,
            "cost_hint": null,
            "can_self_enable": false,
            "setting_namespace": null,
            "setting_key": null,
            "reason": "设备不支持直接播放该视频编码，当前服务未配置实时转码",
            "suggestion": "请更换支持相应格式的播放器",
            "capability": body["capability"],
        });
    }
    let path = std::path::Path::new(&row.path);
    let is_strm = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("strm"));

    // 对于 STRM 虚拟流，前端直接以直通格式播放（统一按通用容器 mp4 处理）
    let container = if is_strm {
        "mp4".to_string()
    } else {
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("mkv")
            .to_string()
    };
    let (audio_tracks, subtitles) = match tracks {
        Some(t) => {
            let a = t
                .audio
                .iter()
                .enumerate()
                .map(|(idx, a)| {
                    let stream_idx = a.stream_index.unwrap_or(idx as u32);
                    json!({
                        "index": stream_idx,
                        "ref": format!("embedded:{stream_idx}"),
                        "codec": a.codec,
                        "language": a.language,
                        "title": a.title,
                        "channels": a.channels,
                        "is_default": a.is_default,
                    })
                })
                .collect();
            let s = t
                .subtitles
                .iter()
                .enumerate()
                .map(|(idx, s)| {
                    let stream_idx = library::subtitle_index(t, idx);
                    let track_ref = if s.is_external {
                        let fname = s
                            .path
                            .as_ref()
                            .and_then(|p| std::path::Path::new(p).file_name())
                            .and_then(|n| n.to_str())
                            .unwrap_or("subtitle");
                        format!("external:{fname}")
                    } else {
                        format!("embedded:{stream_idx}")
                    };
                    let kind = match s.codec.as_deref().unwrap_or("srt").to_lowercase().as_str() {
                        "ass" | "ssa" => "ass",
                        "pgs" | "sup" => "pgs",
                        _ => "vtt",
                    };
                    json!({
                        "index": stream_idx,
                        "track_ref": track_ref,
                        "kind": kind,
                        "codec": s.codec,
                        "language": s.language,
                        "title": s.title,
                        "is_default": s.is_default,
                        "forced": s.forced,
                        "is_external": s.is_external,
                        "delivery_url": format!("/api/v1/playback/subtitles/{}/{}", row.id, stream_idx),
                    })
                })
                .collect();
            (a, s)
        }
        None => (Vec::new(), Vec::new()),
    };
    json!({
        "outcome": "plan",
        "tier": 0,
        "file_id": row.id.to_string(),
        "container": container,
        "video": { "action": "copy", "codec": row.codec, "height": null, "tone_map": false, "burn_subtitle": null },
        "audio": { "action": "copy", "track_ref": null, "codec": null, "channels": null, "downmix": false },
        // 直出整份文件时浏览器只放容器默认音轨，请求里的 audio_track 不会生效。
        // 不把探测到的多条轨交给菜单，避免用户点了之后白重启、仍听到默认轨。
        "audio_tracks": if audio_tracks.len() < 2 { audio_tracks } else { Vec::<Value>::new() },
        "subtitles": subtitles,
        "degraded_from": null,
        "cost_hint": null,
        "can_self_enable": false,
        "setting_namespace": null,
        "setting_key": null,
        "reason": "",
        "suggestion": null,
        "capability": body["capability"],
    })
}

/// POST /playback/decide — tier-0 direct decision only (no transcode).
pub(crate) async fn decide(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Json(body): Json<Value>,
) -> Response {
    let store = state.store.lock();
    let Some((row, _media, _, _)) = row_for(&store, &body, user_id) else {
        return err(StatusCode::NOT_FOUND, "playback.item_missing", "条目不存在");
    };
    let tracks = store.get_file_meta(&row.id.to_string()).ok().flatten();
    ok(direct_decision(&row, &body, tracks.as_ref())).into_response()
}

pub(super) fn unit_for_unit(
    store: &crate::Store,
    user_id: domain::UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> Value {
    match store.unit_state(user_id, media_id, season, episode) {
        Ok(Some(unit)) => unit_json(&unit),
        _ => json!({
            "position_ms": 0, "played": false, "play_count": 0, "duration_ms": null,
            "audio_track": null, "subtitle_track": null, "is_favorite": false,
        }),
    }
}

// ---------------------------------------------------------------------------
// Progress / metrics / policy / client-log
// ---------------------------------------------------------------------------

/// POST /playback/metrics — best-effort quality snapshot (never fails).
pub(crate) async fn metrics(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Json(body): Json<Value>,
) -> Response {
    let now = unix_now();
    let store = state.store.lock();
    let media_id = metric_media_id(&store, user_id, &body);
    match media_id {
        Some(media_id) => {
            if let Err(error) = store.insert_metric(
                user_id,
                media_id,
                body["tier"].as_i64().unwrap_or(0) as i32,
                body["engine"].as_str(),
                body["ttff_ms"].as_i64(),
                body["rebuffer_ms"].as_i64().unwrap_or(0),
                body["rebuffer_count"].as_i64().unwrap_or(0),
                body["seek_count"].as_i64().unwrap_or(0),
                body["dropped_frames"].as_i64(),
                body["total_frames"].as_i64(),
                body["watched_ms"].as_i64().unwrap_or(0),
                now,
            ) {
                tracing::warn!(%error, %user_id, %media_id, "Playback quality metric persistence failed");
            }
        }
        None => tracing::warn!(%user_id, library_file_id = ?body.get("library_file_id"),
            media_item_id = ?body.get("media_item_id"), "Playback quality metric identity is invalid or not visible"),
    }
    ok(json!({ "ok": true })).into_response()
}

fn metric_media_id(
    store: &crate::Store,
    user_id: domain::UserId,
    body: &Value,
) -> Option<domain::MediaId> {
    let explicit = body["media_item_id"].as_str().and_then(resolve_media_id);
    if let Some(file) = body.get("library_file_id").filter(|file| !file.is_null()) {
        let row = crate::http::media_visibility::resolve_visible_row(
            store,
            file.as_str()?,
            Some(user_id),
        )?;
        return explicit
            .is_none_or(|id| id == row.media_id)
            .then_some(row.media_id);
    }
    let id = explicit?;
    let media = store.get_media(id).ok().flatten()?;
    let rows = store.list_ledger().ok()?;
    (!visible_rows_for_media(store, &media, &rows, Some(user_id)).is_empty()).then_some(id)
}

/// GET/PUT /playback/policy — direct-play only; transcode flags persist in KV.
pub(crate) async fn get_policy(State(state): State<ApiState>) -> Response {
    let store = state.store.lock();
    let prefs = store
        .get_setting("playback.policy")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}));
    ok(json!({
        "software_transcode_enabled": prefs["software_transcode_enabled"].as_bool().unwrap_or(false),
        "trickplay_enabled": prefs["trickplay_enabled"].as_bool().unwrap_or(false),
        "transcode_cache_enabled": prefs["transcode_cache_enabled"].as_bool().unwrap_or(false),
        "hardware_available": false,
        "hw_backends": [],
    }))
    .into_response()
}

pub(crate) async fn put_policy(State(state): State<ApiState>, Json(body): Json<Value>) -> Response {
    let store = state.store.lock();
    let mut prefs = store
        .get_setting("playback.policy")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}));
    for key in [
        "software_transcode_enabled",
        "trickplay_enabled",
        "transcode_cache_enabled",
    ] {
        if let Some(value) = body.get(key) {
            prefs[key] = value.clone();
        }
    }
    if let Err(error) = store.put_setting("playback.policy", &prefs.to_string()) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    ok(json!({
        "software_transcode_enabled": prefs["software_transcode_enabled"].as_bool().unwrap_or(false),
        "trickplay_enabled": prefs["trickplay_enabled"].as_bool().unwrap_or(false),
        "transcode_cache_enabled": prefs["transcode_cache_enabled"].as_bool().unwrap_or(false),
        "hardware_available": false,
        "hw_backends": [],
    }))
    .into_response()
}

/// POST /playback/client-log — best-effort client telemetry sink.
pub(crate) async fn client_log(
    State(_state): State<ApiState>,
    Json(_body): Json<Value>,
) -> Response {
    ok(json!({ "ok": true })).into_response()
}

// ---------------------------------------------------------------------------
// UI prefs (unchanged)
// ---------------------------------------------------------------------------

fn ui_preferences_key(user_id: domain::UserId) -> String {
    format!("user.{user_id}.ui.preferences")
}

pub(crate) async fn get_ui_prefs(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
) -> Response {
    let store = state.store.lock();
    let raw = store
        .get_setting(&ui_preferences_key(user_id))
        .ok()
        .flatten();
    let prefs = raw
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or_else(|| json!({}));
    ok(prefs).into_response()
}

pub(crate) async fn put_ui_prefs(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Json(body): Json<Value>,
) -> Response {
    let store = state.store.lock();
    let key = ui_preferences_key(user_id);
    let raw = store.get_setting(&key).ok().flatten();
    let mut prefs = raw
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or_else(|| json!({}));
    if let (Some(obj), Some(new_obj)) = (prefs.as_object_mut(), body.as_object()) {
        for (k, v) in new_obj {
            obj.insert(k.clone(), v.clone());
        }
    }
    if let Err(error) = store.put_setting(&key, &prefs.to_string()) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    ok(prefs).into_response()
}

pub(crate) fn session_timeout_secs() -> i64 {
    SESSION_TIMEOUT_SECS
}

/// GET /playback/hardware — 硬件转码能力（我们不转码：恒空）。
pub(crate) async fn hardware() -> Response {
    ok(json!({
        "hw_backend": null,
        "available": [],
    }))
    .into_response()
}

/// GET /playback/subtitles/{ledger_id}/{index} — 交付已探测的字幕文件
pub(crate) async fn get_subtitle_file(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    axum::extract::Path((ledger_id, index)): axum::extract::Path<(String, u32)>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let (row, media_title, media_kind, tracks) = {
        let store = state.store.lock();
        // G07: 先校验该 ledger 行对当前用户可见，防止普通成员跨 Library 读取字幕。
        let Some(row) =
            crate::http::media_visibility::resolve_visible_row(&store, &ledger_id, Some(user_id))
        else {
            return err(StatusCode::NOT_FOUND, "subtitle.not_found", "字幕未找到");
        };
        let Some(media) = store.get_media(row.media_id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "subtitle.not_found", "字幕未找到");
        };
        // canonical ledger row id（不是 URL 原文）才是 cache 维度与 file_meta key。
        let Ok(Some(tracks)) = store.get_file_meta(&row.id.to_string()) else {
            return err(StatusCode::NOT_FOUND, "subtitle.not_found", "字幕未找到");
        };
        (row, media.title, media.kind, tracks)
    };
    let wants_vtt = query
        .get("format")
        .map(|f| f.eq_ignore_ascii_case("vtt"))
        .unwrap_or(false);
    let request = crate::subtitle_delivery::SubtitleRequest {
        ledger_id: row.id,
        source_path: std::path::PathBuf::from(&row.path),
        kind: media_kind,
        tracks,
        index,
        wants_vtt,
    };

    match state.subtitle_delivery.deliver(&state, request).await {
        Ok(payload) => (
            [(axum::http::header::CONTENT_TYPE, payload.content_type)],
            payload.bytes,
        )
            .into_response(),
        Err(error) => {
            tracing::error!(
                media = %media_title,
                path = %row.path,
                ledger_id = %ledger_id,
                index,
                %error,
                "交付字幕文件失败"
            );
            subtitle_error_response(error)
        }
    }
}

/// 字幕服务错误 → REST 状态码（原有 404/500 语义保留，新增 503/504/409）。
fn subtitle_error_response(error: crate::subtitle_delivery::SubtitleServiceError) -> Response {
    use crate::subtitle_delivery::SubtitleServiceError;
    use library::DeliveryError;
    match error {
        SubtitleServiceError::Delivery(DeliveryError::TrackNotFound) => {
            err(StatusCode::NOT_FOUND, "subtitle.not_found", "字幕轨不存在")
        }
        SubtitleServiceError::Delivery(DeliveryError::FileMissing | DeliveryError::Io(_)) => err(
            StatusCode::NOT_FOUND,
            "subtitle.file_missing",
            "字幕文件不存在",
        ),
        SubtitleServiceError::Delivery(DeliveryError::Extraction(msg)) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "subtitle.extraction_failed",
            &format!("提取内封字幕失败: {msg}"),
        ),
        SubtitleServiceError::Busy => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "subtitle.busy",
            "字幕服务繁忙，请稍后重试",
        ),
        SubtitleServiceError::Timeout => err(
            StatusCode::GATEWAY_TIMEOUT,
            "subtitle.timeout",
            "字幕提取超时",
        ),
        SubtitleServiceError::Cancelled => {
            err(StatusCode::CONFLICT, "subtitle.cancelled", "字幕提取已取消")
        }
        SubtitleServiceError::Worker(message) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "subtitle.worker",
            &format!("字幕提取任务异常: {message}"),
        ),
    }
}
