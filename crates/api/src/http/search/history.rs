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
    parse_history(
        store
            .get_setting(&user_setting_key("search.history", user_id))
            .ok()
            .flatten()
            .as_deref(),
    )
}

/// 解析历史设置原文；坏值/缺键都退化成空列表（与旧行为一致）。
fn parse_history(raw: Option<&str>) -> Vec<Value> {
    raw.and_then(|raw| serde_json::from_str(raw).ok())
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
    if let Err(error) = store.put_setting(
        &user_setting_key("search.history", user_id),
        &serde_json::json!(history).to_string(),
    ) {
        // 快照已经落盘，条目没写上只会丢一条历史入口，不影响「条目⇒快照可回放」。
        tracing::warn!(%error, %user_id, "写入关键词历史条目失败");
    }
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

/// 单条删除的事务计划：历史里没有这个 id 就返回 `None`（不删快照、不改设置）。
fn single_delete_plan(
    current: Option<&str>,
    id: &str,
) -> Option<crate::store::HistoryDeletePlan> {
    let mut history = parse_history(current);
    let before = history.len();
    history.retain(|item| item["id"].as_str() != Some(id));
    if history.len() == before {
        return None;
    }
    Some(crate::store::HistoryDeletePlan {
        snapshot_ids: vec![id.to_string()],
        settings_value: Some(serde_json::json!(history).to_string()),
    })
}

/// 清空的事务计划：删掉当前历史里的全部快照，再删掉设置键本身。
fn clear_plan(current: Option<&str>) -> Option<crate::store::HistoryDeletePlan> {
    let snapshot_ids = parse_history(current)
        .iter()
        .filter_map(|item| item["id"].as_str().map(str::to_string))
        .collect();
    Some(crate::store::HistoryDeletePlan {
        snapshot_ids,
        settings_value: None,
    })
}

/// 按 id 删除一条关键词历史（连同结果快照）。
///
/// 快照行与历史设置都在 app.db，删除由 store 的**单个事务**完成：要么条目和快照
/// 一起删掉，要么都保留。任一步失败整体回滚并返回 `Err`，路由层回 500，绝不会
/// 出现「条目还在、快照已删」的点开即 404。
fn delete_history_entry(
    store: &crate::Store,
    user_id: domain::UserId,
    id: &str,
) -> Result<bool, crate::store::StoreError> {
    let key = user_setting_key("search.history", user_id);
    let outcome = store.delete_search_history(&key, |current| single_delete_plan(current, id))?;
    if outcome.deleted && outcome.snapshots_removed == 0 {
        // 快照本就不在（历史条目在旧版本里可能没落盘快照）：不算失败，但要说清。
        tracing::warn!(history_id = %id, "历史条目没有对应快照，仅清理条目");
    }
    Ok(outcome.deleted)
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
    match delete_history_entry(&store, user_id, &id) {
        Ok(true) => ok(json!({ "deleted": true })).into_response(),
        Ok(false) => err(StatusCode::NOT_FOUND, "search.history", "历史快照不存在"),
        Err(error) => {
            tracing::error!(%error, history_id = %id, "删除历史条目失败");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                "删除历史记录失败，请稍后重试",
            )
        }
    }
}

/// DELETE /search/history — 清空关键词历史与全部快照。
///
/// 与单条删除同源：删快照与删设置键在**同一个 app.db 事务**里完成，任一步失败
/// 整体回滚并回 500，不会留下「条目在、快照没了」的死链，也不会只删掉一半快照。
pub(crate) async fn clear_search_history_route(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
) -> Response {
    let store = state.store.lock();
    let key = user_setting_key("search.history", user_id);
    match store.delete_search_history(&key, clear_plan) {
        Ok(_) => ok(json!({ "cleared": true })).into_response(),
        Err(error) => {
            tracing::error!(%error, %user_id, "清空搜索历史失败");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                "清空搜索历史失败，请稍后重试",
            )
        }
    }
}
