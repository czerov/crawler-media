//! Jobs: defs with schedule, live/last children, manual run/cancel/tick.

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use domain::JobDefId;
use domain::UserId;
use futures_core::Stream;
use jobs::JobKind;
use serde::Deserialize;
use serde_json::{Value, json};
use std::str::FromStr;

use crate::http::{err, ok, ok_list};
use crate::management::ApiState;

fn job_media_title(payload_val: &Value, store: &crate::Store) -> Option<String> {
    if let Some(sub_id_str) = payload_val.get("subscribe_id").and_then(|v| v.as_str()) {
        if let Ok(sub_id) = domain::SubscribeId::from_str(sub_id_str) {
            store
                .get_subscribe(sub_id)
                .ok()
                .flatten()
                .and_then(|sub| store.get_media(sub.media_id).ok().flatten())
                .map(|m| m.title)
        } else {
            None
        }
    } else if let Some(med_id_str) = payload_val.get("media_id").and_then(|v| v.as_str()) {
        if let Ok(med_id) = domain::MediaId::from_str(med_id_str) {
            store.get_media(med_id).ok().flatten().map(|m| m.title)
        } else {
            None
        }
    } else {
        None
    }
}

fn job_friendly_name(name: &str, media_title: Option<&str>) -> String {
    if name.starts_with("search ") || name.starts_with("catalog ") {
        if let Some(title) = media_title {
            if name.starts_with("search ") {
                format!("自动搜索《{title}》")
            } else {
                format!("刷新元数据《{title}》")
            }
        } else {
            name.to_string()
        }
    } else {
        match name {
            "RSS" => "站点 RSS 增量抓取".into(),
            "Transfer" => "媒体文件入库整理".into(),
            "Watch intake" => "监控目录变动吸纳入库".into(),
            "Scrape" => "元数据与封面刮削".into(),
            "Check-in" => "站点每日自动签到".into(),
            _ => name.to_string(),
        }
    }
}

fn job_last_status<'a>(
    live: Option<&'a jobs::Job>,
    last: Option<&'a jobs::Job>,
) -> Option<&'a str> {
    match (live, last) {
        (Some(l), _) if l.status == jobs::JobStatus::Running || l.run_after == 0 => {
            Some(l.status.as_str())
        }
        (_, Some(r)) => Some(r.status.as_str()),
        (Some(l), None) => Some(l.status.as_str()),
        (None, None) => None,
    }
}

/// 渲染 def 需要的 media title（payload → subscribe/media → store）。
fn def_media_title(def: &jobs::JobDef, store: &crate::Store) -> Option<String> {
    let payload_val: Value = serde_json::from_str(&def.payload).unwrap_or_else(|_| json!({}));
    job_media_title(&payload_val, store)
}

/// 纯渲染：调用方在锁外传入预先取好的 media title。
fn def_json(
    def: &jobs::JobDef,
    live: Option<&jobs::Job>,
    last: Option<&jobs::Job>,
    media_title: Option<&str>,
) -> Value {
    let payload_val: Value = serde_json::from_str(&def.payload).unwrap_or_else(|_| json!({}));
    let friendly_name = job_friendly_name(&def.name, media_title);

    json!({
        "id": def.id.to_string(),
        "kind": def.kind.as_str(),
        "name": def.name,
        "friendly_name": friendly_name,
        "enabled": def.enabled,
        "schedule": match def.schedule {
            Some(jobs::Schedule::Interval { secs }) => json!({ "interval_secs": secs }),
            None => Value::Null,
        },
        "payload": payload_val,
        "concurrency_key": def.concurrency_key,
        "last_status": job_last_status(live, last),
        "last_finished_at": last.and_then(|job| job.finished_at).or_else(|| live.and_then(|job| job.finished_at)),
        "last_error": last.and_then(|job| job.error.as_deref()).or_else(|| live.and_then(|job| job.error.as_deref())),
        "next_run_after": if !def.enabled {
            Value::Null
        } else {
            live.map(|job| json!(job.run_after)).unwrap_or_else(|| {
                last.and_then(|job| job.finished_at)
                    .and_then(|finished| def.schedule.as_ref().map(|s| json!(s.next_after(finished))))
                    .unwrap_or(Value::Null)
            })
        },
    })
}

/// 单个 def 的渲染便捷入口（仍需 Store 取 media title）。
fn def_json_with_store(
    def: &jobs::JobDef,
    live: Option<&jobs::Job>,
    last: Option<&jobs::Job>,
    store: &crate::Store,
) -> Value {
    def_json(def, live, last, def_media_title(def, store).as_deref())
}

#[derive(Deserialize)]
pub(crate) struct ListJobsQuery {
    pub scope: Option<String>,
    pub active_only: Option<bool>,
    pub limit: Option<usize>,
}

fn def_visible_to_user(
    def: &jobs::JobDef,
    store: &crate::Store,
    user_id: UserId,
    admin: bool,
) -> bool {
    if admin || def.kind != JobKind::SubscribeSearch {
        return true;
    }
    let Some(raw_id) = serde_json::from_str::<Value>(&def.payload)
        .ok()
        .and_then(|payload| {
            payload
                .get("subscribe_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
    else {
        return false;
    };
    let Ok(subscribe_id) = domain::SubscribeId::from_str(&raw_id) else {
        return false;
    };
    store
        .get_subscribe(subscribe_id)
        .ok()
        .flatten()
        .is_some_and(|subscribe| subscribe.user_id == user_id)
}

pub(crate) async fn list_jobs(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Query(query): Query<ListJobsQuery>,
) -> Response {
    // 1) Queue 快照：defs（单独加锁，不与 Store 嵌套）。
    let defs = {
        let queue = state.jobs.lock();
        match queue.list_defs() {
            Ok(defs) => defs,
            Err(error) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        }
    };
    // 2) Store 快照：可见性与 scope 过滤（都在 limit 之前）。
    let visible_defs: Vec<jobs::JobDef> = {
        let store = state.store.lock();
        let admin = store
            .user_role(user_id)
            .map(|role| role == "admin")
            .unwrap_or(false);
        defs.into_iter()
            .filter(|def| {
                if !def_visible_to_user(def, &store, user_id, admin) {
                    return false;
                }
                // scope=system 仅保留系统级任务，排除条目级任务
                // （subscribe_search / catalog_refresh）。
                if query.scope.as_deref() == Some("system") {
                    let is_individual = def.kind == JobKind::SubscribeSearch
                        || def.kind == JobKind::CatalogRefresh
                        || def.name.starts_with("search ")
                        || def.name.starts_with("catalog ");
                    if is_individual {
                        return false;
                    }
                }
                true
            })
            .collect()
    };
    // 3) Queue 快照：一次批量取所有 live/last child（不再逐 def 查询）。
    let children = {
        let queue = state.jobs.lock();
        let ids: Vec<JobDefId> = visible_defs.iter().map(|def| def.id).collect();
        queue.children_for_defs(&ids).unwrap_or_default()
    };
    let mut by_def: std::collections::HashMap<JobDefId, jobs::JobChildren> = children
        .into_iter()
        .map(|child| (child.def_id, child))
        .collect();
    let mut selected = Vec::with_capacity(visible_defs.len());
    for def in visible_defs {
        let child = by_def.remove(&def.id);
        let (live, last) = match child {
            Some(child) => (child.live, child.last),
            None => (None, None),
        };
        if query.active_only == Some(true) && live.is_none() {
            continue;
        }
        selected.push((def, live, last));
        if let Some(limit) = query.limit {
            if selected.len() >= limit {
                break;
            }
        }
    }
    // 4) Store 快照：只取渲染所需的 media title，锁内不渲染 JSON。
    let titles: Vec<Option<String>> = {
        let store = state.store.lock();
        selected
            .iter()
            .map(|(def, _, _)| def_media_title(def, &store))
            .collect()
    };
    // 5) 锁外渲染。
    let listed: Vec<Value> = selected
        .into_iter()
        .zip(titles)
        .map(|((def, live, last), title)| {
            def_json(&def, live.as_ref(), last.as_ref(), title.as_deref())
        })
        .collect();
    ok_list(listed).into_response()
}

pub(crate) async fn get_job(
    State(state): State<ApiState>,
    Extension(user_id): Extension<UserId>,
    Path(id): Path<String>,
) -> Response {
    let id = match JobDefId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "job.invalid", "任务 id 无效"),
    };
    let queue = state.jobs.lock();
    let defs = match queue.list_defs() {
        Ok(defs) => defs,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let Some(def) = defs.into_iter().find(|d| d.id == id) else {
        return err(StatusCode::NOT_FOUND, "job.missing", "任务不存在");
    };
    let live = queue.get_live_child(def.id).ok().flatten();
    let last = queue.get_last_child(def.id).ok().flatten();
    let store = state.store.lock();
    let admin = store
        .user_role(user_id)
        .map(|role| role == "admin")
        .unwrap_or(false);
    if !def_visible_to_user(&def, &store, user_id, admin) {
        return err(StatusCode::NOT_FOUND, "job.missing", "任务不存在");
    }
    ok(def_json_with_store(
        &def,
        live.as_ref(),
        last.as_ref(),
        &store,
    ))
    .into_response()
}

pub(crate) async fn run_job(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let id = match JobDefId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "job.invalid", "任务 id 无效"),
    };
    let queue = state.jobs.lock();
    let defs = match queue.list_defs() {
        Ok(defs) => defs,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let Some(def) = defs.into_iter().find(|d| d.id == id) else {
        return err(StatusCode::NOT_FOUND, "job.missing", "任务不存在");
    };
    if let Err(error) = queue.ensure_scheduled_for(0, def.id) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "job.enqueue_failed",
            &error.to_string(),
        );
    }
    ok(json!({ "queued": true })).into_response()
}

#[derive(Deserialize)]
pub(crate) struct ToggleJobInput {
    pub enabled: bool,
}

pub(crate) async fn toggle_job(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<ToggleJobInput>,
) -> Response {
    let id = match JobDefId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "job.invalid", "任务 id 无效"),
    };
    let queue = state.jobs.lock();
    let defs = match queue.list_defs() {
        Ok(defs) => defs,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let Some(def) = defs.into_iter().find(|d| d.id == id) else {
        return err(StatusCode::NOT_FOUND, "job.missing", "任务不存在");
    };
    if let Err(error) = queue.set_def_enabled_by_id(def.id, body.enabled) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "job.toggle_failed",
            &error.to_string(),
        );
    }
    if !body.enabled {
        let _ = queue.cancel_def_children(def.id);
    }
    ok(json!({ "id": def.id.to_string(), "enabled": body.enabled })).into_response()
}

#[derive(Deserialize)]
pub(crate) struct UpdateJobScheduleInput {
    pub interval_secs: Option<u64>,
}

pub(crate) async fn update_job_schedule(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateJobScheduleInput>,
) -> Response {
    let id = match JobDefId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "job.invalid", "任务 id 无效"),
    };
    let queue = state.jobs.lock();
    let defs = match queue.list_defs() {
        Ok(defs) => defs,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let Some(def) = defs.into_iter().find(|d| d.id == id) else {
        return err(StatusCode::NOT_FOUND, "job.missing", "任务不存在");
    };

    let new_schedule = match body.interval_secs {
        Some(secs) => {
            if secs == 0 {
                return err(
                    StatusCode::BAD_REQUEST,
                    "job.invalid_interval",
                    "执行周期必须大于 0 秒",
                );
            }
            Some(jobs::Schedule::Interval { secs })
        }
        None => None,
    };

    if let Err(error) = queue.set_def_schedule_by_id(def.id, new_schedule) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "job.schedule_update_failed",
            &error.to_string(),
        );
    }

    // 取消由于旧调度计划在排队中的子任务，并按新 schedule 重新安排下次执行
    let _ = queue.cancel_def_children(def.id);
    let now = crate::job_loop::unix_now();
    let _ = queue.ensure_scheduled(now);

    ok(json!({
        "id": def.id.to_string(),
        "schedule": body.interval_secs.map(|secs| json!({ "interval_secs": secs }))
    }))
    .into_response()
}

pub(crate) async fn cancel_job(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let id = match JobDefId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "job.invalid", "任务 id 无效"),
    };
    let queue = state.jobs.lock();
    match queue.cancel_def_children(id) {
        Ok(count) => ok(json!({ "cancelled": count })).into_response(),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "job.cancel_failed",
            &error.to_string(),
        ),
    }
}

#[derive(Deserialize)]
pub(crate) struct TickQuery {
    now: Option<i64>,
}

pub(crate) async fn tick_jobs(
    State(state): State<ApiState>,
    Query(query): Query<TickQuery>,
) -> Response {
    let requested_now = query.now;
    let now = requested_now.unwrap_or_else(crate::job_loop::unix_now);
    let outcome = tokio::task::spawn_blocking(move || {
        crate::job_loop::run_pending_jobs_with_clock(&state, now, || {
            requested_now.unwrap_or_else(crate::job_loop::unix_now)
        })
    })
    .await;
    match outcome {
        Ok(Ok(ran)) => ok(json!({
            "ran": ran.len(),
            "kinds": ran.iter().map(|job| job.kind.as_str()).collect::<Vec<_>>(),
        }))
        .into_response(),
        Ok(Err(error)) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "job.tick_failed",
            &error.to_string(),
        ),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "job.tick_failed",
            &error.to_string(),
        ),
    }
}

/// SSE stream the activity view subscribes to: emits `ready` on connect and a
/// `job` event whenever the shared count changes.
///
/// 所有连接共用 `JobUpdates` 的单个 2 秒 poller；客户端断开即结束本连接任务。
pub(crate) async fn job_stream(State(state): State<ApiState>) -> Response {
    use tokio_stream::wrappers::ReceiverStream;
    let mut updates = match state.job_updates.subscribe(state.jobs.clone()).await {
        Ok(receiver) => receiver,
        Err(error) => {
            tracing::error!(%error, "任务计数订阅失败");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error);
        }
    };
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, std::convert::Infallible>>(64);
    tokio::spawn(async move {
        if tx
            .send(Ok(Event::default().event("ready").data("{}")))
            .await
            .is_err()
        {
            return;
        }

        // 订阅时已持有真实当前计数：立即下发，不空等 2 秒。
        let mut last = (*updates.borrow()).clone();
        if tx
            .send(Ok(Event::default()
                .event("job")
                .data(format!(r#"{{"count":{}}}"#, last.count))))
            .await
            .is_err()
        {
            return;
        }

        loop {
            tokio::select! {
                changed = updates.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let current = (*updates.borrow()).clone();
                    if current == last {
                        continue;
                    }
                    if tx
                        .send(Ok(Event::default()
                            .event("job")
                            .data(format!(r#"{{"count":{}}}"#, current.count))))
                        .await
                        .is_err()
                    {
                        break;
                    }
                    last = current;
                }
                _ = tx.closed() => break,
            }
        }
    });
    Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::default())
        .into_response()
}
