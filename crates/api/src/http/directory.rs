//! Directory: library roots, Transfer mode, naming templates, watch dirs.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::MediaKind;
use serde::Deserialize;
use serde_json::{Value, json};
use std::str::FromStr;

use crate::http::{err, ok};
use crate::management::ApiState;
use crate::scrape_store::{ScrapeStoreExt, apply_legacy_naming};

#[derive(Deserialize)]
pub(crate) struct DirectoryInput {
    movie_root: String,
    tv_root: String,
    transfer_mode: String,
    movie_naming: String,
    tv_naming: String,
    #[serde(default)]
    scrape: bool,
    #[serde(default)]
    watch_intake: Option<String>,
    #[serde(default)]
    watch_inplace: Option<String>,
}

pub(crate) async fn get_directory(State(state): State<ApiState>) -> Response {
    let store = state.store.lock();
    match directory_json(&store) {
        Ok(value) => ok(value).into_response(),
        Err(error) => err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error),
    }
}

fn directory_json(store: &crate::Store) -> Result<Value, String> {
    let roots = store.list_library_roots().map_err(|e| e.to_string())?;
    let extra_roots: Vec<Value> = roots
        .iter()
        .filter(|root| !root.is_default)
        .map(|root| {
            json!({
                "id": root.id,
                "kind": root.kind.as_str(),
                "path": root.path.display().to_string(),
                "is_default": root.is_default,
            })
        })
        .collect();
    Ok(json!({
        "movie_root": store.library_root(MediaKind::Movie).map_err(|e| e.to_string())?.display().to_string(),
        "tv_root": store.library_root(MediaKind::Tv).map_err(|e| e.to_string())?.display().to_string(),
        "transfer_mode": store.transfer_mode().map_err(|e| e.to_string())?,
        "movie_naming": store.naming_pattern(MediaKind::Movie).map_err(|e| e.to_string())?,
        "tv_naming": store.naming_pattern(MediaKind::Tv).map_err(|e| e.to_string())?,
        "scrape": store.scrape_enabled().map_err(|e| e.to_string())?,
        "watch_intake": store.watch_intake().map_err(|e| e.to_string())?,
        "watch_inplace": store.watch_inplace().map_err(|e| e.to_string())?,
        "movie_root_id": roots.iter().find(|r| r.is_default && r.kind == MediaKind::Movie).map(|r| r.id.clone()),
        "tv_root_id": roots.iter().find(|r| r.is_default && r.kind == MediaKind::Tv).map(|r| r.id.clone()),
        "extra_roots": extra_roots,
    }))
}

pub(crate) async fn put_directory(
    State(state): State<ApiState>,
    Json(body): Json<DirectoryInput>,
) -> Response {
    if !matches!(body.transfer_mode.as_str(), "hardlink" | "copy" | "move") {
        return err(
            StatusCode::BAD_REQUEST,
            "directory.invalid",
            "transfer_mode 必须是 hardlink/copy/move",
        );
    }
    if body.movie_root.trim().is_empty() || body.tv_root.trim().is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "directory.invalid",
            "movie_root 和 tv_root 必填",
        );
    }
    let store = state.store.lock();
    let applied = store.app_write(|s| {
        // Compose one final setting: Movie first, then TV — the shared
        // `naming_entry_dir` follows the existing "TV overrides" rule.
        let mut setting = s.get_scrape_config()?.setting;
        apply_legacy_naming(&mut setting, MediaKind::Movie, body.movie_naming.trim());
        apply_legacy_naming(&mut setting, MediaKind::Tv, body.tv_naming.trim());
        s.set_library_root(MediaKind::Movie, body.movie_root.trim())?;
        s.set_library_root(MediaKind::Tv, body.tv_root.trim())?;
        s.set_transfer_mode(&body.transfer_mode)?;
        s.set_scrape_enabled(body.scrape)?;
        if let Some(path) = body.watch_intake.as_deref() {
            s.set_watch_intake(path)?;
        }
        if let Some(path) = body.watch_inplace.as_deref() {
            s.set_watch_inplace(path)?;
        }
        s.save_scrape_config(&setting)?;
        Ok(())
    });
    if let Err(error) = applied {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    match directory_json(&store) {
        Ok(value) => ok(value).into_response(),
        Err(error) => err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error),
    }
}

#[derive(Deserialize)]
pub(crate) struct RootInput {
    kind: String,
    path: String,
}

pub(crate) async fn add_root(
    State(state): State<ApiState>,
    Json(body): Json<RootInput>,
) -> Response {
    if body.path.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "directory.invalid", "路径必填");
    }
    let kind = match MediaKind::from_str(&body.kind) {
        Ok(kind) => kind,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "directory.invalid",
                "kind 必须是 movie 或 tv",
            );
        }
    };
    let store = state.store.lock();
    match store.insert_library_root(kind, body.path.trim()) {
        Ok(root) => (
            StatusCode::CREATED,
            axum::Json(json!({
                "ok": true,
                "data": {
                    "id": root.id,
                    "kind": root.kind.as_str(),
                    "path": root.path.display().to_string(),
                    "is_default": root.is_default,
                }
            })),
        )
            .into_response(),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

pub(crate) async fn delete_root(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let store = state.store.lock();
    let Some(root) = store.get_library_root(&id).ok().flatten() else {
        return err(
            StatusCode::NOT_FOUND,
            "directory.root_missing",
            "目录不存在",
        );
    };
    if root.is_default {
        return err(
            StatusCode::BAD_REQUEST,
            "directory.protected",
            "不能删除默认库根",
        );
    }
    match store.delete_extra_library_root(&id) {
        Ok(true) => ok(json!({ "deleted": true })).into_response(),
        _ => err(
            StatusCode::NOT_FOUND,
            "directory.root_missing",
            "目录不存在",
        ),
    }
}
