//! Live media-library activity: active playback sessions, unattributed
//! downloads (stream transfers without a session), devices, and the admin
//! end/revoke actions.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::http::playback::session_timeout_secs;
use crate::http::playback_device_target::{DeviceTargetQuery, resolve_device_target};
use crate::http::{err, ok, ok_list};
use crate::management::ApiState;
use crate::playback_read::PlaybackRead;
use crate::store::SessionRow;

/// 统一使用注入的 `PlaybackClock`：测试可用假时钟推进活动/设备/结束/撤销路径，
/// 生产下与 `unix_now()` 等价。
fn playback_now(state: &ApiState) -> i64 {
    state.playback_clock.now_ms() / 1000
}

fn session_json(session: &SessionRow, media: Value, member_name: &str) -> Value {
    let percent = session
        .duration_ms
        .filter(|d| *d > 0)
        .map(|d| (session.position_ms as f64 / d as f64 * 100.0).round() as i64)
        .filter(|p| (0..=100).contains(p));
    json!({
        "user_id": session.user_id.to_string(),
        "device_id": session.device_id,
        "revocable": session.revocable(),
        "member_name": member_name,
        "client": session.client,
        "device_name": session.device_name,
        "client_version": session.client_version,
        "media": media,
        "position_ms": session.position_ms,
        "duration_ms": session.duration_ms,
        "progress_percent": percent,
        "paused": session.paused,
        "play_method": session.play_method,
        "rate_bytes_per_second": session.rate_bps,
        "bytes_sent": session.bytes_sent,
        "connections": session.connections,
        "file": {
            "resolution": null,
            "video_codec": null,
            "hdr": null,
            "container": null,
            "bit_rate": null,
            "size_bytes": null,
        },
        "started_at": iso(session.started_at),
        "last_report_at": iso(session.last_report_at),
    })
}

fn iso(unix_secs: i64) -> String {
    let millis = unix_secs as i128 * 1000;
    format!("{millis}")
}

/// GET /playback/activity?scope=visible|all — active sessions + downloads.
/// scope=visible folds sessions whose media lives in libraries the requester
/// cannot browse into hidden counts (admins: admin_visible=false; members:
/// everyone/selected).
pub(crate) async fn activity(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let scope = query.get("scope").map(String::as_str).unwrap_or("visible");
    let now = playback_now(&state);
    let store = state.store.lock();
    let admin = user_id
        .and_then(|id| store.user_role(id).ok())
        .is_some_and(|role| role == "admin");
    let sessions: Vec<SessionRow> = store
        .active_sessions(now, session_timeout_secs())
        .unwrap_or_default()
        .into_iter()
        .filter(|session| !session.admin_ended)
        .filter(|session| admin || user_id == Some(session.user_id))
        .collect();
    let media_ids: Vec<domain::MediaId> = sessions.iter().map(|session| session.media_id).collect();
    let user_ids: Vec<domain::UserId> = sessions.iter().map(|session| session.user_id).collect();
    let read = load_read(&store, &media_ids, &user_ids);
    let mut hidden = 0usize;
    let items: Vec<Value> = sessions
        .iter()
        .filter_map(|session| {
            let visible = session_library_visible(
                &read,
                &store,
                session,
                user_id,
                if admin { scope } else { "visible" },
            );
            if visible {
                Some(session_json(
                    session,
                    crate::playback_read::media_target(
                        &read,
                        session.media_id,
                        session.season,
                        session.episode,
                    ),
                    &read.member_name(session.user_id),
                ))
            } else {
                hidden += 1;
                None
            }
        })
        .collect();
    ok(json!({
        "sessions": items,
        "downloads": [],
        "hidden_session_count": hidden,
        "hidden_download_count": 0,
    }))
    .into_response()
}

fn load_read(
    store: &crate::Store,
    media_ids: &[domain::MediaId],
    user_ids: &[domain::UserId],
) -> PlaybackRead {
    match PlaybackRead::load(store, media_ids, user_ids) {
        Ok(read) => read,
        Err(error) => {
            tracing::error!(%error, "播放读快照加载失败，按不可见/占位处理");
            PlaybackRead::empty()
        }
    }
}

fn session_library_visible(
    read: &PlaybackRead,
    store: &crate::Store,
    session: &SessionRow,
    user_id: Option<domain::UserId>,
    scope: &str,
) -> bool {
    if scope == "all" {
        return true;
    }
    let Some(media) = read.media(session.media_id) else {
        return false;
    };
    read.rows_for(session.media_id)
        .iter()
        .any(|row| crate::http::library::row_visible_to_user(store, row, media, user_id))
}

/// POST /playback/activity/sessions/{device_id}/end?user_id=... — mark one
/// user's session admin-ended; the next progress report exits the player.
pub(crate) async fn end_session(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(device_id): Path<String>,
    Query(query): Query<DeviceTargetQuery>,
) -> Response {
    let now = playback_now(&state);
    let store = state.store.lock();
    let target_user = match resolve_device_target(&store, user_id, &device_id, &query) {
        Ok(target_user) => target_user,
        Err(response) => return response,
    };
    let Some(mut session) = store.get_session(target_user, &device_id).ok().flatten() else {
        return err(
            StatusCode::NOT_FOUND,
            "playback.session_missing",
            "没有该设备的活跃会话",
        );
    };
    session.admin_ended = true;
    session.last_report_at = now;
    if let Err(error) = store.upsert_session(&session) {
        tracing::error!(%error, %target_user, device_id, "结束设备会话失败");
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            "结束播放会话失败，请稍后重试",
        );
    }
    ok(json!({ "ended": 1 })).into_response()
}

/// GET /playback/devices — distinct devices from sessions + play logs.
pub(crate) async fn devices(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
) -> Response {
    let now = playback_now(&state);
    let store = state.store.lock();
    let admin = store
        .user_role(user_id)
        .map(|role| role == "admin")
        .unwrap_or(false);
    let target_user = (!admin).then_some(user_id);
    let sessions: Vec<SessionRow> = store
        .active_sessions(now, session_timeout_secs())
        .unwrap_or_default()
        .into_iter()
        .filter(|session| !session.admin_ended)
        .filter(|session| admin || session.user_id == user_id)
        .collect();
    let logs = store
        .list_logs(500, None, None, None, now, target_user)
        .unwrap_or_default();
    let user_ids: Vec<domain::UserId> = sessions
        .iter()
        .map(|session| session.user_id)
        .chain(logs.iter().map(|log| log.user_id))
        .collect();
    let names = load_read(&store, &[], &user_ids);
    let mut seen: HashMap<String, Value> = HashMap::new();
    for session in &sessions {
        let key = format!("{}:{}", session.user_id, session.device_id);
        let entry = seen.entry(key).or_insert_with(|| {
            json!({
                "user_id": session.user_id.to_string(),
                "device_id": session.device_id,
                "member_name": names.member_name(session.user_id),
                "client": session.client,
                "device_name": session.device_name,
                "client_version": session.client_version,
                "revocable": session.revocable() && store.device_is_revocable(session.user_id, &session.device_id).unwrap_or(false),
                "active_sessions": 0,
                "last_seen_at": iso(session.last_report_at),
            })
        });
        entry["active_sessions"] = json!(entry["active_sessions"].as_i64().unwrap_or(0) + 1);
    }
    for log in logs {
        let device_id = log
            .device_id
            .clone()
            .unwrap_or_else(|| format!("legacy-log:{}", log.id));
        let key = format!("{}:{device_id}", log.user_id);
        seen.entry(key).or_insert_with(|| {
            json!({
                "user_id": log.user_id.to_string(),
                "device_id": device_id,
                "member_name": names.member_name(log.user_id),
                "client": log.client,
                "device_name": log.device_name,
                "client_version": null,
                "revocable": log.revocable() && store.device_is_revocable(log.user_id, &device_id).unwrap_or(false),
                "active_sessions": 0,
                "last_seen_at": iso(log.ended_at),
            })
        });
    }
    let mut items: Vec<Value> = seen.into_values().collect();
    items.sort_by_key(|v| v["last_seen_at"].as_str().unwrap_or("0").to_string());
    items.reverse();
    ok_list(items).into_response()
}

/// DELETE /playback/devices/{device_id}?user_id=... — revoke one user's device:
/// end its session and block protocol events/streams. Web supports ending only.
pub(crate) async fn revoke_device(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(device_id): Path<String>,
    Query(query): Query<DeviceTargetQuery>,
) -> Response {
    let now = playback_now(&state);
    let store = state.store.lock();
    let target_user = match resolve_device_target(&store, user_id, &device_id, &query) {
        Ok(target_user) => target_user,
        Err(response) => return response,
    };
    match store.device_is_revocable(target_user, &device_id) {
        Ok(true) => {}
        Ok(false) => {
            tracing::error!(%target_user, device_id, "Playback device credential revocation unsupported");
            return err(
                StatusCode::BAD_REQUEST,
                "playback.unsupported",
                "该设备仅使用系统 CLI 凭据或尚无已绑定的会话凭据，不能安全注销，仅可结束当前播放",
            );
        }
        Err(error) => {
            tracing::error!(%error, %target_user, device_id, "Playback device identity lookup failed");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                "查询设备状态失败",
            );
        }
    }
    if let Err(error) = store.end_device_session(target_user, &device_id, now) {
        tracing::error!(%error, %target_user, device_id, "注销设备前结束会话失败");
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            "注销设备失败，请稍后重试",
        );
    }
    if let Err(error) = store.revoke_device(target_user, &device_id) {
        tracing::error!(%error, %target_user, device_id, "撤销设备凭据失败");
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            "注销设备失败，请稍后重试",
        );
    }
    ok(json!({ "revoked": true })).into_response()
}
