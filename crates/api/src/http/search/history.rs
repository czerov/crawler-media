use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::http::{err, ok};
use crate::management::ApiState;

/// 记录关键词历史并返回该条目的 id（同时用作结果快照 id）。
fn user_setting_key(base: &str, user_id: domain::UserId) -> String {
    format!("user.{user_id}.{base}")
}

fn history_items(store: &crate::Store, user_id: domain::UserId) -> Vec<Value> {
    store
        .get_setting(&user_setting_key("search.history", user_id))
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// 一次搜索要写的历史/快照目标。
///
/// 顺序是**先写快照、再写历史条目**：这样「历史里有条目 ⇒ 快照一定可取」，
/// 客户端中断或快照落盘失败都不会留下点开就 404 的死链。
pub(super) struct HistoryTarget {
    pub id: String,
    pub user_id: domain::UserId,
}

/// 预先分配快照 id（不做任何写库动作），真正的历史条目在快照落盘后才写入。
pub(super) fn next_history_target(user_id: domain::UserId) -> HistoryTarget {
    let id = format!(
        "{user_id}:{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    HistoryTarget { id, user_id }
}

/// 写入关键词历史条目（快照已经落盘时才调用）。
fn record_history_entry(
    store: &crate::Store,
    user_id: domain::UserId,
    query: &str,
    provider: &str,
    id: &str,
) {
    let mut history = history_items(store, user_id);
    history.retain(|item| item["query"].as_str() != Some(query));
    history.insert(
        0,
        json!({
            "id": id,
            "query": query,
            "provider": provider,
            "created_at": chrono_like_now(),
        }),
    );
    history.truncate(20);
    let _ = store.put_setting(
        &user_setting_key("search.history", user_id),
        &serde_json::json!(history).to_string(),
    );
}

/// 落盘 torrents 快照并在成功后补写历史条目；失败时不写历史（宁可不记录也不留死链）。
pub(super) fn save_torrents_history(
    state: &ApiState,
    target: Option<&HistoryTarget>,
    keyword: &str,
    total: i64,
    elapsed_ms: Option<i64>,
    items: &[Value],
    sites: &[Value],
    partial: bool,
) -> bool {
    let Some(target) = target else { return false };
    let payload = json!({
        "keyword": keyword,
        "snapshot_at": chrono_like_now(),
        "total": total,
        "elapsed_ms": elapsed_ms,
        "items": items,
        "sites": sites,
        "partial": partial,
    });
    let store = state.store.lock();
    match store.insert_search_snapshot(
        &target.id,
        "torrents",
        keyword,
        &payload.to_string(),
        crate::job_loop::unix_now(),
    ) {
        Ok(()) => {
            record_history_entry(&store, target.user_id, keyword, "torrents", &target.id);
            true
        }
        Err(error) => {
            tracing::error!(%error, history_id = %target.id, "保存搜索结果快照失败，不写历史条目");
            false
        }
    }
}

/// 落盘 titles 快照并在成功后补写历史条目（与 torrents 同一不变式）。
pub(super) fn save_titles_history(
    state: &ApiState,
    target: Option<&HistoryTarget>,
    keyword: &str,
    provider: &str,
    titles: &[Value],
    providers: &[Value],
) -> bool {
    let Some(target) = target else { return false };
    let payload = json!({
        "query": keyword,
        "provider": provider,
        "snapshot_at": chrono_like_now(),
        "total": titles.len() as i64,
        "titles": titles,
        "providers": providers,
        "partial": false,
    });
    let store = state.store.lock();
    match store.insert_search_snapshot(
        &target.id,
        "titles",
        keyword,
        &payload.to_string(),
        crate::job_loop::unix_now(),
    ) {
        Ok(()) => {
            record_history_entry(&store, target.user_id, keyword, "titles", &target.id);
            true
        }
        Err(error) => {
            tracing::error!(%error, history_id = %target.id, "保存标题搜索快照失败，不写历史条目");
            false
        }
    }
}

/// 按 id 删除一条关键词历史（连同结果快照）。
fn delete_history_entry(store: &crate::Store, user_id: domain::UserId, id: &str) -> bool {
    let mut history = history_items(store, user_id);
    let before = history.len();
    history.retain(|item| item["id"].as_str() != Some(id));
    if history.len() == before {
        return false;
    }
    let _ = store.put_setting(
        &user_setting_key("search.history", user_id),
        &serde_json::json!(history).to_string(),
    );
    let _ = store.delete_search_snapshot(id);
    true
}

pub(super) fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Civil-from-days for the Unix epoch offset (valid for our range).
    let days = secs / 86400;
    let rem = secs % 86400;
    let h = rem / 3600;
    let m = (rem % 3600) / 60;
    let sec = rem % 60;
    // 1970-01-01 + days, month/day via cumulative days per month.
    let month_days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut year = 1970i64;
    let mut d = days as i64;
    loop {
        let ydays = if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
            366
        } else {
            365
        };
        if d < ydays {
            break;
        }
        d -= ydays;
        year += 1;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let mut month = 0usize;
    let mut day = d;
    for (i, mdays) in month_days.iter().enumerate() {
        let dim = if i == 1 && leap { 29 } else { *mdays };
        if day < dim as i64 {
            month = i + 1;
            break;
        }
        day -= dim as i64;
    }
    format!(
        "{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{sec:02}Z",
        day = day + 1
    )
}

pub(crate) async fn search_history(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
) -> Response {
    let store = state.store.lock();
    let history = history_items(&store, user_id);
    ok(json!({ "items": history })).into_response()
}

pub(crate) async fn get_presets(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
) -> Response {
    let store = state.store.lock();
    let raw = store
        .get_setting(&user_setting_key("search.presets", user_id))
        .ok()
        .flatten();
    let presets = raw
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or_else(|| json!([]));
    ok(json!({ "presets": presets })).into_response()
}

pub(crate) async fn put_presets(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Json(body): Json<Value>,
) -> Response {
    let presets = body.get("presets").cloned().unwrap_or_else(|| json!([]));
    if let Err(error) = state.store.lock().put_setting(
        &user_setting_key("search.presets", user_id),
        &presets.to_string(),
    ) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    ok(json!({ "presets": presets })).into_response()
}

/// GET /search/history/{id}?vertical=torrents|titles — 回放一次历史搜索的结果快照。
pub(crate) async fn get_history_snapshot(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let store = state.store.lock();
    if !history_items(&store, user_id)
        .iter()
        .any(|item| item["id"].as_str() == Some(&id))
    {
        return err(StatusCode::NOT_FOUND, "search.history", "历史快照不存在");
    }
    let Some((vertical, _, payload, _)) = store.get_search_snapshot(&id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "search.history", "历史快照不存在");
    };
    let want = query
        .get("vertical")
        .map(String::as_str)
        .unwrap_or("torrents");
    if vertical != want {
        return err(StatusCode::NOT_FOUND, "search.history", "历史快照不存在");
    }
    match serde_json::from_str::<Value>(&payload) {
        Ok(value) => ok(json!({
            "vertical": vertical,
            "history_id": id,
            "snapshot_at": value["snapshot_at"].clone(),
            "total": value["total"].clone(),
            "items": value.get("items").cloned().or_else(|| value.get("titles").cloned()).unwrap_or_else(|| json!([])),
            "sites": value.get("sites").cloned().unwrap_or_else(|| json!([])),
            "keyword": value.get("keyword").cloned().or_else(|| value.get("query").cloned()).unwrap_or_else(|| json!(null)),
            "elapsed_ms": value.get("elapsed_ms").cloned().unwrap_or(Value::Null),
            // 客户端中断时落盘的半截快照：可回放，但要如实告诉调用方结果不完整。
            "partial": value.get("partial").cloned().unwrap_or(json!(false)),
        }))
        .into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", "快照损坏"),
    }
}

/// DELETE /search/history/{id} — 删除单条历史（含快照）。
pub(crate) async fn delete_history_entry_route(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
) -> Response {
    let store = state.store.lock();
    if delete_history_entry(&store, user_id, &id) {
        ok(json!({ "deleted": true })).into_response()
    } else {
        err(StatusCode::NOT_FOUND, "search.history", "历史快照不存在")
    }
}

/// DELETE /search/history — 清空关键词历史与全部快照。
pub(crate) async fn clear_search_history_route(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
) -> Response {
    let store = state.store.lock();
    let history = history_items(&store, user_id);
    for id in history.iter().filter_map(|item| item["id"].as_str()) {
        let _ = store.delete_search_snapshot(id);
    }
    let _ = store.delete_setting(&user_setting_key("search.history", user_id));
    ok(json!({ "cleared": true })).into_response()
}
