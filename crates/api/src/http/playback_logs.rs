//! Playback history + watch statistics, aggregated from `playback_logs`.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::str::FromStr;

use crate::http::playback::media_target_json;
use crate::http::{err, ok};
use crate::job_loop::unix_now;
use crate::management::ApiState;
use crate::store::PlayLogRow;

fn log_json(store: &crate::Store, log: &PlayLogRow, member_name: &str) -> Value {
    let media = media_target_json(store, log.media_id, log.season, log.episode);
    let progress = log
        .duration_ms
        .filter(|d| *d > 0)
        .map(|d| (log.end_position_ms as f64 / d as f64 * 100.0).round() as i64)
        .filter(|p| (0..=100).contains(p));
    json!({
        "id": log.id,
        "member_name": member_name,
        "media": media,
        "client": log.client,
        "device_name": log.device_name,
        "started_at": millis(log.started_at),
        "ended_at": millis(log.ended_at),
        "watched_ms": log.watched_ms,
        "start_position_ms": log.start_position_ms,
        "end_position_ms": log.end_position_ms,
        "duration_ms": log.duration_ms,
        "progress_percent": progress,
        "completed": log.completed,
    })
}

fn millis(unix_secs: i64) -> String {
    (unix_secs as i128 * 1000).to_string()
}

pub(super) fn member_name(store: &crate::Store, user_id: domain::UserId) -> String {
    store
        .get_user(user_id)
        .ok()
        .flatten()
        .map(|user| user.login)
        .unwrap_or_else(|| "成员".into())
}

/// Resolve the frontend's member filter: a user UUID string, or "0"/"" = admin
/// (first admin user by role, not by alphabetical order).
pub(super) fn member_filter(store: &crate::Store, raw: Option<&str>) -> Option<domain::UserId> {
    let raw = raw?;
    // Try parsing as UUID first (direct user id).
    if let Ok(uid) = domain::UserId::from_str(raw) {
        return Some(uid);
    }
    // Legacy numeric id: 0 = first admin, n = nth user by id.
    let member_id: i64 = raw.parse().ok()?;
    let users = store.list_users().unwrap_or_default();
    if member_id <= 0 {
        // Find the first admin user instead of relying on alphabetical sort.
        users
            .iter()
            .find(|u| store.user_role(u.id).ok().as_deref() == Some("admin"))
            .or(users.first())
            .map(|u| u.id)
    } else {
        users.get((member_id - 1) as usize).map(|user| user.id)
    }
}

pub(super) fn visible_media_ids(
    store: &crate::Store,
    user_id: domain::UserId,
    scope: &str,
) -> Option<std::collections::HashSet<domain::MediaId>> {
    if scope == "all" {
        return None;
    }
    Some(
        store
            .list_ledger()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|row| {
                let media = store.get_media(row.media_id).ok().flatten()?;
                crate::http::library::row_visible_to_user(store, &row, &media, Some(user_id))
                    .then_some(row.media_id)
            })
            .collect(),
    )
}

pub(super) fn media_visible(
    visible_media: Option<&std::collections::HashSet<domain::MediaId>>,
    media_id: domain::MediaId,
) -> bool {
    visible_media.is_none_or(|ids| ids.contains(&media_id))
}

/// GET /playback/history?limit&before&days&member_id&scope — cursor-paginated.
pub(crate) async fn history(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let limit: usize = query
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(30)
        .clamp(1, 200);
    let before = query
        .get("before")
        .and_then(|v| v.parse::<i128>().ok())
        .map(|ms| (ms / 1000) as i64);
    let days: Option<i64> = query.get("days").and_then(|v| v.parse().ok());
    let now = unix_now();
    let store = state.store.lock();
    let admin = store
        .user_role(user_id)
        .map(|role| role == "admin")
        .unwrap_or(false);
    let target_user = if admin {
        member_filter(&store, query.get("member_id").map(String::as_str))
    } else {
        Some(user_id)
    };
    let scope = if admin && query.get("scope").is_some_and(|scope| scope == "all") {
        "all"
    } else {
        "visible"
    };
    let visible_media = visible_media_ids(&store, user_id, scope);
    // SQL 侧做 keyset 分页 + 可见集合过滤 + hidden 计数：不再把整个表读进内存。
    let log_scope = crate::store::PlaybackLogScope {
        before,
        // Store 对「只有 before_id 没有 before」返回 Invalid；旧实现忽略，保持一致。
        before_id: if before.is_some() {
            query.get("before_id").map(String::clone)
        } else {
            None
        },
        since: days.map(|days| now - days * 86_400),
        user_id: target_user,
        visible_media: visible_media.map(|ids| ids.into_iter().collect()),
    };
    let page = match store.playback_log_page(&log_scope, limit) {
        Ok(page) => page,
        Err(error) => {
            tracing::error!(%error, "播放历史分页失败");
            crate::store::PlaybackLogPage {
                entries: Vec::new(),
                has_more: false,
                hidden_count: 0,
            }
        }
    };
    let entries: Vec<Value> = page
        .entries
        .iter()
        .map(|log| log_json(&store, log, &member_name(&store, log.user_id)))
        .collect();
    // Composite cursor: encode both started_at (ms) and id to avoid
    // skipping same-second records at the page boundary.
    let cursor_log = page.entries.last().filter(|_| page.has_more);
    let next_cursor = cursor_log.map(|log| (log.started_at as i128 * 1000).to_string());
    let next_cursor_id = cursor_log.map(|log| log.id.clone());
    ok(json!({
        "entries": entries,
        "hidden_count": page.hidden_count,
        "has_more": page.has_more,
        "next_cursor": next_cursor,
        "next_cursor_id": next_cursor_id,
    }))
    .into_response()
}

fn resolve_history_media_ids(
    store: &crate::Store,
    scope: &str,
    query: &HashMap<String, String>,
) -> Result<Option<Vec<domain::MediaId>>, Response> {
    match scope {
        "all" => Ok(None),
        "item" => {
            let Some(raw_id) = query.get("media_item_id") else {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "playback.invalid_scope",
                    "scope=item 缺少 media_item_id",
                ));
            };
            let id = domain::MediaId::from_str(raw_id).map_err(|_| {
                err(
                    StatusCode::BAD_REQUEST,
                    "playback.invalid_id",
                    "media_item_id 格式无效",
                )
            })?;
            Ok(Some(vec![id]))
        }
        "library" => {
            let Some(library_id) = query.get("library_id") else {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "playback.invalid_scope",
                    "scope=library 缺少 library_id",
                ));
            };
            let library = store
                .get_library(library_id)
                .map_err(|e: store::StoreError| {
                    err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "store.error",
                        &e.to_string(),
                    )
                })?
                .ok_or_else(|| err(StatusCode::NOT_FOUND, "library.missing", "媒体库不存在"))?;
            let mut ids = Vec::new();
            let ledger = store.list_ledger().map_err(|e: store::StoreError| {
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &e.to_string(),
                )
            })?;
            for row in ledger {
                if library
                    .root_paths
                    .iter()
                    .any(|root| std::path::Path::new(&row.path).starts_with(root))
                    && !ids.contains(&row.media_id)
                {
                    ids.push(row.media_id);
                }
            }
            Ok(Some(ids))
        }
        _ => Err(err(
            StatusCode::BAD_REQUEST,
            "playback.invalid_scope",
            "未知 scope，仅支持 item, library, all",
        )),
    }
}

/// DELETE /playback/history?scope=item|library|all&media_item_id&library_id&since
/// — clear the caller's own units/logs/metrics.
pub(crate) async fn clear_history(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let since_ms = query.get("since").and_then(|v| v.parse::<i128>().ok());
    let since = since_ms.map(|ms| (ms / 1000) as i64);
    let scope = query.get("scope").map(String::as_str).unwrap_or("all");
    let store = state.store.lock();
    let media_ids = match resolve_history_media_ids(&store, scope, &query) {
        Ok(ids) => ids,
        Err(resp) => return resp,
    };
    let (deleted_states, deleted_metrics) = match store.playback_write(|store| {
        let states = store.clear_units(user_id, media_ids.as_deref(), since)?
            + store.delete_logs(user_id, media_ids.as_deref(), since)?;
        let metrics = store.delete_metrics(user_id, media_ids.as_deref(), since)?;
        Ok((states, metrics))
    }) {
        Ok(counts) => counts,
        Err(error) => {
            tracing::error!(%error, %user_id, scope, "Playback history clear failed");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                "清除观看记录失败",
            );
        }
    };
    ok(json!({
        "deleted_states": deleted_states,
        "deleted_metrics": deleted_metrics,
    }))
    .into_response()
}

#[allow(dead_code)]
fn _err_shape() -> Response {
    err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", "")
}
