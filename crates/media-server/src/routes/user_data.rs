//! Jellyfin/Emby per-item user data: favorite and played marks.

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::Value;

use crate::auth::AuthUser;

use super::websocket;
use super::{AppState, UserItemPath, item_id_for_user};

async fn user_data_response(
    state: &AppState,
    user_id: domain::UserId,
    item_id: &str,
) -> Result<Json<Value>, StatusCode> {
    let snapshot = state
        .provider
        .resolve_user_item_data(user_id, item_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, %user_id, %item_id, "failed to resolve Jellyfin user data response");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(snapshot.json_value()))
}

pub(super) async fn mark_favorite(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserItemPath>,
) -> Result<Json<Value>, StatusCode> {
    let id = item_id_for_user(state.provider.as_ref(), path, user.id).await?;
    let updated = state
        .provider
        .set_user_marks(user.id, &id, None, Some(true))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !updated {
        return Err(StatusCode::NOT_FOUND);
    }
    tracing::info!(user_id = %user.id, item_id = %id, favorite = true, "updated Jellyfin favorite mark");
    let state_clone = state.clone();
    let id_clone = id.clone();
    tokio::spawn(async move {
        websocket::publish_user_data_changed(&state_clone, user.id, &id_clone).await;
    });
    user_data_response(&state, user.id, &id).await
}

pub(super) async fn unmark_favorite(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserItemPath>,
) -> Result<Json<Value>, StatusCode> {
    let id = item_id_for_user(state.provider.as_ref(), path, user.id).await?;
    let updated = state
        .provider
        .set_user_marks(user.id, &id, None, Some(false))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !updated {
        return Err(StatusCode::NOT_FOUND);
    }
    tracing::info!(user_id = %user.id, item_id = %id, favorite = false, "updated Jellyfin favorite mark");
    let state_clone = state.clone();
    let id_clone = id.clone();
    tokio::spawn(async move {
        websocket::publish_user_data_changed(&state_clone, user.id, &id_clone).await;
    });
    user_data_response(&state, user.id, &id).await
}

pub(super) async fn mark_played(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserItemPath>,
) -> Result<Json<Value>, StatusCode> {
    let id = item_id_for_user(state.provider.as_ref(), path, user.id).await?;
    let updated = state
        .provider
        .set_user_marks(user.id, &id, Some(true), None)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !updated {
        return Err(StatusCode::NOT_FOUND);
    }
    tracing::info!(user_id = %user.id, item_id = %id, played = true, "updated Jellyfin played mark");
    let state_clone = state.clone();
    let id_clone = id.clone();
    tokio::spawn(async move {
        websocket::publish_user_data_changed(&state_clone, user.id, &id_clone).await;
    });
    user_data_response(&state, user.id, &id).await
}

pub(super) async fn unmark_played(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserItemPath>,
) -> Result<Json<Value>, StatusCode> {
    let id = item_id_for_user(state.provider.as_ref(), path, user.id).await?;
    let updated = state
        .provider
        .set_user_marks(user.id, &id, Some(false), None)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !updated {
        return Err(StatusCode::NOT_FOUND);
    }
    tracing::info!(user_id = %user.id, item_id = %id, played = false, "updated Jellyfin played mark");
    let state_clone = state.clone();
    let id_clone = id.clone();
    tokio::spawn(async move {
        websocket::publish_user_data_changed(&state_clone, user.id, &id_clone).await;
    });
    user_data_response(&state, user.id, &id).await
}
