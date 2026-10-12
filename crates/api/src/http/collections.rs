//! 用户自建收藏合集：CRUD + 条目 + 排序 + 图廊。

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::http::library::{library_for_row, library_visible, poster_path, preferred_row};
use crate::http::{err, ok, ok_list};
use crate::management::ApiState;
use std::str::FromStr;

fn ensure_collection_owner(
    store: &crate::Store,
    collection_id: &str,
    user_id: domain::UserId,
) -> Result<(), Response> {
    match store.collection_owned_by(collection_id, user_id) {
        Ok(true) => Ok(()),
        Ok(false) => Err(err(
            StatusCode::NOT_FOUND,
            "collection.missing",
            "合集不存在",
        )),
        Err(error) => Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        )),
    }
}

struct VisibleCollectionItem {
    media: domain::Media,
    library_id: String,
    rows: Vec<domain::LedgerRow>,
}

fn visible_collection_item(
    store: &crate::Store,
    media_id: domain::MediaId,
    user_id: domain::UserId,
) -> Option<VisibleCollectionItem> {
    let media = store.get_media(media_id).ok().flatten()?;
    let rows = store.list_ledger().ok()?;
    let library = rows
        .iter()
        .filter(|row| row.media_id == media_id)
        .filter_map(|row| library_for_row(store, row, &media))
        .find(|library| library_visible(store, library, Some(user_id)))?;
    let rows = rows
        .into_iter()
        .filter(|row| {
            row.media_id == media_id
                && library_for_row(store, row, &media).is_some_and(|owner| owner.id == library.id)
        })
        .collect();
    Some(VisibleCollectionItem {
        media,
        library_id: library.id,
        rows,
    })
}

fn visible_collection_items(
    store: &crate::Store,
    collection_id: &str,
    user_id: domain::UserId,
) -> Vec<VisibleCollectionItem> {
    store
        .collection_item_ids(collection_id)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|raw| domain::MediaId::from_str(&raw).ok())
        .filter_map(|media_id| visible_collection_item(store, media_id, user_id))
        .collect()
}

fn poster_url(rows: &[domain::LedgerRow]) -> Option<String> {
    let row = preferred_row(rows)?;
    poster_path(row)?;
    Some(crate::http::library::artwork_url("posters", row.id))
}

/// GET /collections — 我的合集（含封面与条目数）。
pub(crate) async fn list_collections(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
) -> Response {
    let store = state.store.lock();
    let items: Vec<Value> = store
        .list_collections(user_id)
        .unwrap_or_default()
        .into_iter()
        .map(|collection| {
            let items = visible_collection_items(&store, &collection.id, user_id);
            json!({
                "id": collection.id,
                "name": collection.name,
                "item_count": items.len(),
                "cover_url": items.iter().find_map(|item| poster_url(&item.rows)),
            })
        })
        .collect();
    ok_list(items).into_response()
}

/// POST /collections — 新建。
pub(crate) async fn create_collection(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Json(body): Json<Value>,
) -> Response {
    let name = body["name"].as_str().unwrap_or("").trim().to_string();
    if name.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "collection.invalid",
            "合集名不能为空",
        );
    }
    let store = state.store.lock();
    match store.create_collection(user_id, &name) {
        Ok(id) => ok(json!({ "id": id })).into_response(),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

/// PATCH /collections/{id} — 重命名。
pub(crate) async fn rename_collection(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let name = body["name"].as_str().unwrap_or("").trim().to_string();
    if name.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "collection.invalid",
            "合集名不能为空",
        );
    }
    let store = state.store.lock();
    if let Err(response) = ensure_collection_owner(&store, &id, user_id) {
        return response;
    }
    match store.rename_collection(&id, &name) {
        Ok(true) => ok(json!({ "renamed": true })).into_response(),
        Ok(false) => err(StatusCode::NOT_FOUND, "collection.missing", "合集不存在"),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

/// DELETE /collections/{id}
pub(crate) async fn delete_collection(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
) -> Response {
    let store = state.store.lock();
    if let Err(response) = ensure_collection_owner(&store, &id, user_id) {
        return response;
    }
    match store.delete_collection(&id) {
        Ok(true) => ok(json!({ "deleted": true })).into_response(),
        Ok(false) => err(StatusCode::NOT_FOUND, "collection.missing", "合集不存在"),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

/// GET /collections/{id}/items — 条目（库 items 同形状）。
pub(crate) async fn collection_items(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
) -> Response {
    let store = state.store.lock();
    if let Err(response) = ensure_collection_owner(&store, &id, user_id) {
        return response;
    }
    let items: Vec<Value> = visible_collection_items(&store, &id, user_id)
        .into_iter()
        .map(|item| {
            json!({
                "media_item_id": item.media.id.to_string(),
                "library_id": item.library_id,
                "kind": item.media.kind.as_str(),
                "title": item.media.title,
                "year": item.media.year,
                "poster_url": poster_url(&item.rows),
            })
        })
        .collect();
    ok_list(items).into_response()
}

/// POST /collections/{id}/items — 添加条目。
pub(crate) async fn add_collection_item(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let media_item_id = body["media_item_id"].as_str().unwrap_or_default();
    let media_id = match domain::MediaId::from_str(media_item_id) {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "collection.invalid",
                "media_item_id 无效",
            );
        }
    };
    let store = state.store.lock();
    if let Err(response) = ensure_collection_owner(&store, &id, user_id) {
        return response;
    }
    if visible_collection_item(&store, media_id, user_id).is_none() {
        return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
    }
    match store.add_collection_item(&id, media_item_id) {
        Ok(added) => ok(json!({ "added": added })).into_response(),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

/// DELETE /collections/{id}/items/{media_item_id}
pub(crate) async fn remove_collection_item(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path((id, media_item_id)): Path<(String, String)>,
) -> Response {
    let store = state.store.lock();
    if let Err(response) = ensure_collection_owner(&store, &id, user_id) {
        return response;
    }
    match store.remove_collection_item(&id, &media_item_id) {
        Ok(removed) => ok(json!({ "removed": removed })).into_response(),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

/// PUT /collections/{id}/order — 条目排序。
pub(crate) async fn reorder_collection(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let ids: Vec<String> = body["media_item_ids"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let store = state.store.lock();
    if let Err(response) = ensure_collection_owner(&store, &id, user_id) {
        return response;
    }
    match store.reorder_collection_items(&id, &ids) {
        Ok(_) => ok(json!({ "reordered": true })).into_response(),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

/// GET /collections/{id}/gallery — 图廊（LibraryGalleryGroup 形状）。
pub(crate) async fn collection_gallery(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
) -> Response {
    let store = state.store.lock();
    if let Err(response) = ensure_collection_owner(&store, &id, user_id) {
        return response;
    }
    let groups: Vec<Value> = visible_collection_items(&store, &id, user_id)
        .into_iter()
        .map(|item| {
            let mut images = Vec::new();
            if let Some(row) = preferred_row(&item.rows) {
                if let Some(dir) = std::path::Path::new(&row.path).parent() {
                    if dir.join("poster.jpg").is_file() {
                        images.push(json!({
                            "kind": "poster", "url": crate::http::library::artwork_url("posters", row.id),
                            "aspect": 0.667, "label": "海报",
                            "season": null, "episode": null, "t_seconds": null,
                        }));
                    }
                    if dir.join("fanart.jpg").is_file() {
                        images.push(json!({
                            "kind": "backdrop", "url": crate::http::library::artwork_url("fanart", row.id),
                            "aspect": 1.778, "label": "背景",
                            "season": null, "episode": null, "t_seconds": null,
                        }));
                    }
                }
            }
            // 真实收藏态：whole unit；movie 沿用既有 legacy (0,0) 兜底。
            let is_favorite = crate::http::playback::movie_compatible_state(
                &store,
                user_id,
                item.media.id,
                crate::store::UNIT_WHOLE,
                crate::store::UNIT_WHOLE,
            )
            .ok()
            .flatten()
            .is_some_and(|unit| unit.favorite);
            json!({
                "media_item_id": item.media.id.to_string(),
                "library_id": item.library_id,
                "kind": item.media.kind.as_str(),
                "title": item.media.title,
                "year": item.media.year,
                "is_favorite": is_favorite,
                "images": images,
            })
        })
        .collect();
    ok_list(groups).into_response()
}
