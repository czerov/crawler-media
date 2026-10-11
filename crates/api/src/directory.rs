use std::str::FromStr;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use domain::MediaKind;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::management::{ApiError, ApiState};
use crate::scrape_store::ScrapeStoreExt;

pub async fn get_directory(State(state): State<ApiState>) -> Result<Json<Value>, ApiError> {
    let store = state.store.lock();
    Ok(Json(directory_json(&store)?))
}
#[derive(Deserialize)]
pub struct DirectoryBody {
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

pub async fn put_directory(
    State(state): State<ApiState>,
    Json(body): Json<DirectoryBody>,
) -> Result<Json<Value>, ApiError> {
    if !matches!(body.transfer_mode.as_str(), "hardlink" | "copy" | "move") {
        return Err(ApiError::invalid(
            "directory.invalid",
            "invalid Transfer mode".into(),
        ));
    }
    if body.movie_root.trim().is_empty() || body.tv_root.trim().is_empty() {
        return Err(ApiError::invalid(
            "directory.invalid",
            "Library root path required".into(),
        ));
    }
    if body.movie_naming.trim().is_empty() || body.tv_naming.trim().is_empty() {
        return Err(ApiError::invalid(
            "directory.invalid",
            "Naming template required".into(),
        ));
    }
    let store = state.store.lock();
    store.set_library_root(MediaKind::Movie, body.movie_root.trim())?;
    store.set_library_root(MediaKind::Tv, body.tv_root.trim())?;
    store.set_legacy_naming(MediaKind::Movie, body.movie_naming.trim())?;
    store.set_legacy_naming(MediaKind::Tv, body.tv_naming.trim())?;
    store.set_transfer_mode(&body.transfer_mode)?;
    store.set_scrape_enabled(body.scrape)?;
    if let Some(path) = &body.watch_intake {
        store.set_watch_intake(path)?;
    }
    if let Some(path) = &body.watch_inplace {
        store.set_watch_inplace(path)?;
    }
    Ok(Json(directory_json(&store)?))
}

fn directory_json(store: &crate::Store) -> Result<Value, ApiError> {
    let roots = store.list_library_roots()?;
    let extra_roots: Vec<Value> = roots
        .iter()
        .filter(|root| !root.is_default)
        .cloned()
        .map(root_json)
        .collect();
    let movie_id = roots
        .iter()
        .find(|root| root.is_default && root.kind == MediaKind::Movie)
        .map(|root| root.id.clone());
    let tv_id = roots
        .iter()
        .find(|root| root.is_default && root.kind == MediaKind::Tv)
        .map(|root| root.id.clone());
    Ok(json!({
        "movie_root": store.library_root(MediaKind::Movie)?.display().to_string(),
        "tv_root": store.library_root(MediaKind::Tv)?.display().to_string(),
        "movie_root_id": movie_id,
        "tv_root_id": tv_id,
        "transfer_mode": store.transfer_mode()?,
        "movie_naming": store.naming_pattern(MediaKind::Movie)?,
        "tv_naming": store.naming_pattern(MediaKind::Tv)?,
        "scrape": store.scrape_enabled()?,
        "watch_intake": store.watch_intake()?,
        "watch_inplace": store.watch_inplace()?,
        "extra_roots": extra_roots,
    }))
}

#[derive(Deserialize)]
pub struct ExtraRootBody {
    kind: String,
    path: String,
}

pub async fn add_library_root(
    State(state): State<ApiState>,
    Json(body): Json<ExtraRootBody>,
) -> Result<impl IntoResponse, ApiError> {
    if body.path.trim().is_empty() {
        return Err(ApiError::invalid(
            "directory.invalid",
            "Library root path required".into(),
        ));
    }
    let kind = MediaKind::from_str(&body.kind)
        .map_err(|_| ApiError::invalid("directory.invalid", "kind must be movie or tv".into()))?;
    let root = state
        .store
        .lock()
        .insert_library_root(kind, body.path.trim())?;
    Ok((StatusCode::CREATED, Json(root_json(root))))
}

pub async fn delete_library_root(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let store = state.store.lock();
    let Some(root) = store.get_library_root(&id)? else {
        return Err(ApiError::missing(
            "directory.root_missing",
            "Library root not found".into(),
        ));
    };
    if root.is_default {
        return Err(ApiError::invalid(
            "directory.protected",
            "cannot delete the default Library root".into(),
        ));
    }
    store.delete_extra_library_root(&id)?;
    Ok(Json(json!({ "id": id, "deleted": true })))
}

fn root_json(root: crate::store::LibraryRoot) -> Value {
    json!({
        "id": root.id,
        "kind": root.kind.as_str(),
        "path": root.path.display().to_string(),
        "is_default": root.is_default,
    })
}

pub fn parse_transfer_mode(raw: &str) -> Option<library::TransferMode> {
    match raw {
        "hardlink" => Some(library::TransferMode::Hardlink),
        "copy" => Some(library::TransferMode::Copy),
        "move" => Some(library::TransferMode::Move),
        _ => None,
    }
}

pub fn transfer_plan(
    store: &crate::Store,
    kind: MediaKind,
) -> Result<(std::path::PathBuf, Option<library::TransferMode>, bool), crate::store::StoreError> {
    transfer_plan_for_library(store, kind, None)
}

pub fn transfer_plan_for_library(
    store: &crate::Store,
    kind: MediaKind,
    library_id: Option<&str>,
) -> Result<(std::path::PathBuf, Option<library::TransferMode>, bool), crate::store::StoreError> {
    let root = if let Some(id) = library_id {
        if let Some(lib) = store.get_library(id)? {
            if !store.library_accepts_work(id)? {
                return Err(crate::store::StoreError::Protected("目标 Library 正在删除".into()));
            }
            if let Some(r) = lib.root_paths.first() {
                r.clone()
            } else {
                return Err(crate::store::StoreError::Missing("目标 Library 没有根目录".into()));
            }
        } else {
            return Err(crate::store::StoreError::Missing("目标 Library 已删除，请重新选择".into()));
        }
    } else {
        store.library_root(kind)?
    };
    let mode = parse_transfer_mode(&store.transfer_mode()?);
    Ok((root, mode, store.scrape_enabled()?))
}
