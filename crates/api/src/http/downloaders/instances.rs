//! Downloaders: qBittorrent / Transmission instances, live tasks, submit.

use std::str::FromStr;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::DownloaderId;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::http::{err, ok, ok_list};
use crate::management::ApiState;

use super::tasks::{run_downloader_action, run_downloader_read};

#[derive(Deserialize)]
pub(crate) struct DownloaderInput {
    name: String,
    kind: String,
    url: String,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    path_maps: Vec<PathMapInput>,
    #[serde(default)]
    is_default: bool,
    /// Optional toggle; absent means "leave unchanged".
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Deserialize)]
pub(crate) struct PathMapInput {
    from: String,
    to: String,
}

fn downloader_json_with_store(row: &crate::store::DownloaderRow, store: &crate::Store) -> Value {
    let verified = store
        .get_setting(&format!("downloader.{}.verified", row.id))
        .ok()
        .flatten();
    let last_error = store
        .get_setting(&format!("downloader.{}.last_error", row.id))
        .ok()
        .flatten();
    let status = if !row.enabled {
        "disabled"
    } else if verified.as_deref() == Some("true") {
        "active"
    } else if verified.as_deref() == Some("false") {
        "failed"
    } else {
        "pending"
    };
    json!({
        "id": row.id.to_string(),
        "name": row.name,
        "kind": row.kind,
        "url": row.url,
        "username": row.username,
        "category": row.category,
        "path_maps": row.path_maps.iter().map(|m| json!({"from": m.from, "to": m.to.display().to_string()})).collect::<Vec<_>>(),
        "is_default": row.is_default,
        "enabled": row.enabled,
        "status": status,
        "last_error": last_error,
    })
}

fn downloader_json(row: &crate::store::DownloaderRow) -> Value {
    json!({
        "id": row.id.to_string(),
        "name": row.name,
        "kind": row.kind,
        "url": row.url,
        "username": row.username,
        "category": row.category,
        "path_maps": row.path_maps.iter().map(|m| json!({"from": m.from, "to": m.to.display().to_string()})).collect::<Vec<_>>(),
        "is_default": row.is_default,
        "enabled": row.enabled,
        "status": if !row.enabled {
            "disabled"
        } else {
            "pending"
        },
        "last_error": null,
    })
}

fn parse_path_maps(input: Vec<PathMapInput>) -> Vec<downloader::PathMap> {
    input
        .into_iter()
        .filter(|m| !m.from.is_empty() && !m.to.is_empty())
        .map(|m| downloader::PathMap::new(m.from, m.to))
        .collect()
}

pub(crate) async fn list_downloaders(State(state): State<ApiState>) -> Response {
    let store = state.store.lock();
    let rows = match store.list_downloaders() {
        Ok(rows) => rows,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    ok_list(
        rows.iter()
            .map(|r| downloader_json_with_store(r, &store))
            .collect(),
    )
    .into_response()
}

pub(crate) async fn create_downloader(
    State(state): State<ApiState>,
    Json(body): Json<DownloaderInput>,
) -> Response {
    if body.name.trim().is_empty() || body.url.trim().is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "downloader.invalid",
            "名称和 URL 必填",
        );
    }
    if !matches!(body.kind.as_str(), "qbittorrent" | "transmission") {
        return err(
            StatusCode::BAD_REQUEST,
            "downloader.invalid",
            "kind 必须是 qbittorrent 或 transmission",
        );
    }
    let row = crate::store::DownloaderRow {
        id: DownloaderId::new(),
        name: body.name,
        kind: body.kind,
        url: body.url,
        username: body.username.filter(|u| !u.is_empty()),
        password: body.password.filter(|p| !p.is_empty()),
        category: body.category.filter(|c| !c.is_empty()),
        path_maps: parse_path_maps(body.path_maps),
        is_default: body.is_default,
        enabled: body.enabled.unwrap_or(true),
    };
    let store = state.store.lock();
    if let Err(error) = store.insert_downloader(&row) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    if row.is_default {
        if let Err(error) = store.set_default_downloader(row.id) {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    }
    (StatusCode::CREATED, ok(downloader_json(&row))).into_response()
}

pub(crate) async fn get_downloader(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let id = match DownloaderId::from_str(&id) {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "downloader.invalid",
                "下载器 id 无效",
            );
        }
    };
    let store = state.store.lock();
    let Some(row) = store.get_downloader(id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "downloader.missing", "下载器不存在");
    };
    ok(downloader_json(&row)).into_response()
}

#[derive(Deserialize)]
pub(crate) struct DownloaderPatch {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    path_maps: Option<Vec<PathMapInput>>,
    #[serde(default)]
    is_default: Option<bool>,
    #[serde(default)]
    enabled: Option<bool>,
}

fn apply_downloader_patch(row: &mut store::DownloaderRow, body: DownloaderPatch) {
    if let Some(name) = body.name.filter(|name| !name.is_empty()) {
        row.name = name;
    }
    if let Some(url) = body.url.filter(|url| !url.is_empty()) {
        row.url = url;
    }
    if let Some(kind) = body.kind.filter(|kind| !kind.is_empty()) {
        row.kind = kind;
    }
    if let Some(username) = body.username {
        row.username = (!username.is_empty()).then_some(username);
    }
    if let Some(password) = body.password {
        row.password = (!password.is_empty()).then_some(password);
    }
    if let Some(category) = body.category {
        row.category = (!category.is_empty()).then_some(category);
    }
    if let Some(path_maps) = body.path_maps {
        row.path_maps = parse_path_maps(path_maps);
    }
    if let Some(enabled) = body.enabled {
        row.enabled = enabled;
    }
}

pub(crate) async fn patch_downloader(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<DownloaderPatch>,
) -> Response {
    let id = match DownloaderId::from_str(&id) {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "downloader.invalid",
                "下载器 id 无效",
            );
        }
    };
    let store = state.store.lock();
    let Some(mut row) = store.get_downloader(id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "downloader.missing", "下载器不存在");
    };
    let make_default = body.is_default == Some(true);
    apply_downloader_patch(&mut row, body);
    if let Err(error) = store.save_downloader(&row) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    // Updating configuration invalidates previous verification state
    let _ = store.delete_setting(&format!("downloader.{}.verified", row.id));
    let _ = store.delete_setting(&format!("downloader.{}.last_error", row.id));
    if make_default {
        if let Err(error) = store.set_default_downloader(row.id) {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    }
    ok(downloader_json(&row)).into_response()
}

pub(crate) async fn delete_downloader(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let id = match DownloaderId::from_str(&id) {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "downloader.invalid",
                "下载器 id 无效",
            );
        }
    };
    let store = state.store.lock();
    let _ = store.delete_setting(&format!("downloader.{id}.verified"));
    let _ = store.delete_setting(&format!("downloader.{id}.last_error"));
    match store.delete_downloader(id) {
        Ok(true) => {
            // D-L4: its in-flight pending rows can no longer be tracked.
            let orphaned = store.mark_pending_orphaned(id).unwrap_or_else(|error| {
                tracing::warn!(%id, %error, "标记已删除下载器的 pending 失败");
                0
            });
            drop(store);
            if orphaned > 0 {
                tracing::warn!(%id, orphaned, "下载器已删除，其在途任务标记为 orphan");
                crate::worker::notify(
                    &state,
                    "下载器已删除",
                    &format!(
                        "{orphaned} 个在途订阅任务失去下载器，已停止跟踪，可重新搜索提交到其它下载器"
                    ),
                );
            }
            ok(json!({ "deleted": true, "orphaned_pending": orphaned })).into_response()
        }
        _ => err(StatusCode::NOT_FOUND, "downloader.missing", "下载器不存在"),
    }
}

pub(crate) async fn verify_downloader(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let id = match DownloaderId::from_str(&id) {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "downloader.invalid",
                "下载器 id 无效",
            );
        }
    };
    let row = {
        let store = state.store.lock();
        let Some(row) = store.get_downloader(id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "downloader.missing", "下载器不存在");
        };
        row
    };
    let url = row.url.clone();
    let username = row.username.clone();
    let password = row.password.clone();
    let category = row.category.clone();
    let path_maps = row.path_maps.clone();
    let kind = row.kind.clone();
    let runner: Box<dyn FnOnce() -> Result<(), String> + Send + 'static> = match kind.as_str() {
        "qbittorrent" => Box::new(move || {
            downloader::QbitDownloader::connect(downloader::QbitConfig {
                url,
                username: username.unwrap_or_default(),
                password: password.unwrap_or_default(),
                category,
                path_maps,
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
        }),
        "transmission" => Box::new(move || {
            downloader::TransmissionDownloader::connect(downloader::TransmissionConfig {
                url,
                username,
                password,
                path_maps,
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
        }),
        _ => {
            return err(
                StatusCode::BAD_REQUEST,
                "downloader.invalid",
                "未知下载器类型",
            );
        }
    };
    let task = tokio::task::spawn_blocking(move || runner());
    let result = task.await;
    match result {
        Ok(Ok(_)) => {
            let store = state.store.lock();
            let _ = store.put_setting(&format!("downloader.{id}.verified"), "true");
            let _ = store.delete_setting(&format!("downloader.{id}.last_error"));
            ok(json!({ "ok": true, "error": null })).into_response()
        }
        Ok(Err(error)) => {
            let store = state.store.lock();
            let _ = store.put_setting(&format!("downloader.{id}.verified"), "false");
            let _ = store.put_setting(&format!("downloader.{id}.last_error"), &error);
            tracing::error!(id = %id, error = %error, "下载器验证连接失败");
            ok(json!({ "ok": false, "error": error })).into_response()
        }
        Err(join_err) => {
            let err_msg = join_err.to_string();
            let store = state.store.lock();
            let _ = store.put_setting(&format!("downloader.{id}.verified"), "false");
            let _ = store.put_setting(&format!("downloader.{id}.last_error"), &err_msg);
            tracing::error!(id = %id, error = %join_err, "下载器验证连接任务异常中止");
            ok(json!({ "ok": false, "error": err_msg })).into_response()
        }
    }
}
pub(crate) async fn target_prefs(State(state): State<ApiState>) -> Response {
    let store = state.store.lock();
    let raw = store.get_setting("download.target_prefs").ok().flatten();
    let prefs = raw
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or_else(|| json!({}));
    ok(prefs).into_response()
}

pub(crate) async fn put_target_pref(
    State(state): State<ApiState>,
    Path(category): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let store = state.store.lock();
    let raw = store.get_setting("download.target_prefs").ok().flatten();
    let mut prefs = raw
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or_else(|| json!({}));
    if body.is_null() {
        if let Some(obj) = prefs.as_object_mut() {
            obj.remove(&category);
        }
    } else {
        prefs[category] = body;
    }
    if let Err(error) = store.put_setting("download.target_prefs", &prefs.to_string()) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    ok(prefs).into_response()
}

#[derive(Deserialize, Default)]
pub(crate) struct LimitsQuery {
    id: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct SetLimitsInput {
    #[serde(default)]
    download_limit_bytes: Option<Value>,
    #[serde(default)]
    upload_limit_bytes: Option<Value>,
}

fn parse_limit_val(val: Option<&Value>) -> Result<u64, &'static str> {
    match val {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Number(n)) => {
            if let Some(u) = n.as_u64() {
                Ok(u)
            } else {
                Err("限速数值必须为非负整数")
            }
        }
        _ => Err("限速格式不正确，必须为数字或 null"),
    }
}

/// 只解析 query 里的下载器 id；client 解析必须留到 blocking 闭包内（同步建连）。
fn parse_downloader_id(
    id: Option<&str>,
) -> Result<Option<DownloaderId>, (StatusCode, &'static str, String)> {
    match id.filter(|s| !s.is_empty()) {
        Some(raw) => Ok(Some(DownloaderId::from_str(raw).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "downloader.invalid_id",
                "下载器 ID 不是合法 UUID".to_string(),
            )
        })?)),
        None => Ok(None),
    }
}

/// GET /downloaders/limits
pub(crate) async fn get_limits(
    State(state): State<ApiState>,
    Query(query): Query<LimitsQuery>,
) -> Response {
    let downloader_id = match parse_downloader_id(query.id.as_deref()) {
        Ok(id) => id,
        Err((status, code, msg)) => return err(status, code, &msg),
    };
    match run_downloader_read(&state, downloader_id, |client| client.get_speed_limits()).await {
        Ok((dl, ul)) => ok(json!({
            "download_limit_bytes": if dl == 0 { Value::Null } else { json!(dl) },
            "upload_limit_bytes": if ul == 0 { Value::Null } else { json!(ul) },
        }))
        .into_response(),
        Err(error) => error.response(),
    }
}

/// PUT /downloaders/limits
pub(crate) async fn set_limits(
    State(state): State<ApiState>,
    Query(query): Query<LimitsQuery>,
    Json(body): Json<SetLimitsInput>,
) -> Response {
    let dl = match parse_limit_val(body.download_limit_bytes.as_ref()) {
        Ok(v) => v,
        Err(msg) => return err(StatusCode::BAD_REQUEST, "downloader.invalid_limit", msg),
    };
    let ul = match parse_limit_val(body.upload_limit_bytes.as_ref()) {
        Ok(v) => v,
        Err(msg) => return err(StatusCode::BAD_REQUEST, "downloader.invalid_limit", msg),
    };
    let downloader_id = match parse_downloader_id(query.id.as_deref()) {
        Ok(id) => id,
        Err((status, code, msg)) => return err(status, code, &msg),
    };
    // 有副作用：run_to_completion 保证返回时动作已真正结束。
    match run_downloader_action(&state, downloader_id, move |client| {
        client.set_speed_limits(dl, ul)
    })
    .await
    {
        Ok(()) => ok(json!({
            "download_limit_bytes": if dl == 0 { Value::Null } else { json!(dl) },
            "upload_limit_bytes": if ul == 0 { Value::Null } else { json!(ul) },
        }))
        .into_response(),
        Err(error) => error.response(),
    }
}
