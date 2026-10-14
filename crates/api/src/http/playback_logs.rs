//! Playback history + watch statistics, aggregated from `playback_logs`.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::str::FromStr;

use crate::http::{err, ok};
use crate::management::ApiState;
use crate::store::PlayLogRow;

/// 渲染一条历史：`media` target 由调用方批量物化后传入（本函数不再查库）。
fn log_json(media: Value, log: &PlayLogRow, member_name: &str) -> Value {
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

/// 渲染一页历史条目；`read` 为 `None`（快照加载失败）时走未关联 media 的降级渲染。
fn render_entries(
    store: &crate::Store,
    read: Option<&crate::playback_read::PlaybackRead>,
    logs: &[PlayLogRow],
) -> Vec<Value> {
    logs.iter()
        .map(|log| {
            let (media, name) = match read {
                Some(read) => (
                    crate::playback_read::media_target(read, log.media_id, log.season, log.episode),
                    read.member_name(log.user_id),
                ),
                None => (
                    json!({ "media_item_id": log.media_id.to_string(), "title": "", "kind": "movie" }),
                    member_name(store, log.user_id),
                ),
            };
            log_json(media, log, &name)
        })
        .collect()
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
) -> Result<Option<std::collections::HashSet<domain::MediaId>>, store::StoreError> {
    if scope == "all" {
        return Ok(None);
    }
    let vs = crate::visible_scope::VisibleScope::load(store, user_id)?;
    Ok(Some(vs.media_ids.into_iter().collect()))
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
    let before = match query.get("before") {
        Some(s) => match s.parse::<i64>() {
            Ok(ms) => Some(ms.checked_div(1000).unwrap_or(0)),
            Err(_) => return err(StatusCode::BAD_REQUEST, "playback.invalid_cursor", "游标时间戳无效"),
        },
        None => None,
    };
    let days: Option<i64> = match query.get("days") {
        Some(s) => match s.parse::<i64>() {
            Ok(d) if d >= 0 => {
                if d > 36500 || d.checked_mul(86_400).is_none() {
                    return err(StatusCode::BAD_REQUEST, "playback.invalid_days", "天数过滤参数超出有效范围");
                }
                Some(d)
            }
            _ => return err(StatusCode::BAD_REQUEST, "playback.invalid_days", "天数过滤参数无效"),
        },
        None => None,
    };
    // `days` 窗口基准走注入时钟；`since` 是**秒**（`started_at >= since`）。
    let now = state.playback_clock.now_ms() / 1000;
    let since = days.and_then(|d| d.checked_mul(86_400)).map(|secs| now.saturating_sub(secs));
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
    let visible_media = match visible_media_ids(&store, user_id, scope) {
        Ok(v) => v,
        Err(error) => {
            tracing::error!(%error, "加载播放历史可见范围失败");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "playback.query_failed",
                "播放历史查询失败",
            );
        }
    };
    // SQL 侧做 keyset 分页 + 可见集合过滤 + hidden 计数：不再把整个表读进内存。
    let log_scope = crate::store::PlaybackLogScope {
        before,
        // Store 对「只有 before_id 没有 before」返回 Invalid；旧实现忽略，保持一致。
        before_id: if before.is_some() {
            query.get("before_id").map(String::clone)
        } else {
            None
        },
        since,
        user_id: target_user,
        visible_media: visible_media.map(|ids| ids.into_iter().collect()),
    };
    let page = match store.playback_log_page(&log_scope, limit) {
        Ok(page) => page,
        Err(error) => {
            tracing::error!(%error, "播放历史分页失败");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "playback.query_failed",
                "播放历史查询失败",
            );
        }
    };
    // 本页所有 entry 的 media/成员名一次物化；渲染阶段不再查库。
    let media_ids: Vec<domain::MediaId> = page.entries.iter().map(|log| log.media_id).collect();
    let user_ids: Vec<domain::UserId> = page.entries.iter().map(|log| log.user_id).collect();
    let read = match crate::playback_read::PlaybackRead::load(&store, &media_ids, &user_ids) {
        Ok(read) => Some(read),
        Err(error) => {
            tracing::error!(
                %error,
                %user_id,
                media_count = media_ids.len(),
                "加载播放历史媒体快照失败，本次历史报告降级为未关联 media"
            );
            None
        }
    };
    let entries: Vec<Value> = render_entries(&store, read.as_ref(), &page.entries);
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
