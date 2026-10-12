use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::auth::authenticate;
use crate::provider::MediaServerProvider;

mod catalog;
mod item_filters;
mod media;
mod parent_scope;
mod people;
mod sessions;
mod shows;
mod system;
mod user_data;
mod users;
mod websocket;

use catalog::{
    genre_by_name, genres, item_by_id, items, latest_items, resume_items, search_hints,
    studio_by_name, studios,
};
use media::playback_info;
use sessions::{session_playing, session_progress, session_stopped};
use shows::{show_episodes, show_seasons, shows_next_up};
use system::{
    authenticate_by_name, device_description, ping, public_system_info, quickconnect_enabled,
};
use user_data::{mark_favorite, mark_played, unmark_favorite, unmark_played};
use users::{
    display_preferences, item_counts, media_folders, update_display_preferences, user_profile,
    user_views, virtual_folders,
};

pub use system::AuthBody;

#[derive(Clone)]
pub struct AppState {
    pub provider: Arc<dyn MediaServerProvider>,
    data_changes: broadcast::Sender<(domain::UserId, Value)>,
}

impl AppState {
    pub(super) fn new(provider: Arc<dyn MediaServerProvider>) -> Self {
        let (data_changes, _) = broadcast::channel(128);
        Self {
            provider,
            data_changes,
        }
    }
}
#[derive(Default, Deserialize)]
pub struct ItemsQuery {
    #[serde(default, rename = "ParentId", alias = "parentId")]
    pub parent_id: Option<String>,
    #[serde(default, rename = "StartIndex", alias = "startIndex")]
    pub start_index: Option<usize>,
    #[serde(default, rename = "Limit", alias = "limit")]
    pub limit: Option<usize>,
    #[serde(default, rename = "SeriesId", alias = "seriesId")]
    pub series_id: Option<String>,
    #[serde(default, rename = "SeasonId", alias = "seasonId")]
    pub season_id: Option<String>,
    #[serde(
        default,
        rename = "PersonIds",
        alias = "personIds",
        alias = "PersonId",
        alias = "personId"
    )]
    pub person_ids: Option<String>,
    #[serde(default, rename = "Filters", alias = "filters")]
    pub filters: Option<String>,
    #[serde(default, rename = "IsFavorite", alias = "isFavorite")]
    pub is_favorite: Option<bool>,
    #[serde(default, rename = "IsPlayed", alias = "isPlayed")]
    pub is_played: Option<bool>,
    #[serde(default, rename = "SortBy", alias = "sortBy")]
    pub sort_by: Option<String>,
    #[serde(default, rename = "SortOrder", alias = "sortOrder")]
    pub sort_order: Option<String>,
    #[serde(default, rename = "IncludeItemTypes", alias = "includeItemTypes")]
    pub include_item_types: Option<String>,
    #[serde(default, rename = "Recursive", alias = "recursive")]
    pub recursive: Option<bool>,
    #[serde(default, rename = "IsFolder", alias = "isFolder")]
    pub is_folder: Option<bool>,
    #[serde(default, rename = "SearchTerm", alias = "searchTerm")]
    pub search_term: Option<String>,
    #[serde(default, rename = "Genres", alias = "genres", alias = "Genre", alias = "genre")]
    pub genres: Option<String>,
    #[serde(default, rename = "GenreIds", alias = "genreIds", alias = "GenreId", alias = "genreId")]
    pub genre_ids: Option<String>,
    #[serde(default, rename = "StudioIds", alias = "studioIds", alias = "Studios", alias = "studios")]
    pub studio_ids: Option<String>,
}
#[derive(Deserialize)]
pub(super) struct UserPath {
    #[serde(default)]
    pub(super) user_id: Option<String>,
}
#[derive(Deserialize)]
struct UserItemPath {
    user_id: Option<String>,
    id: String,
}
async fn item_id_for_user(
    provider: &dyn MediaServerProvider,
    path: UserItemPath,
    user_id: domain::UserId,
) -> Result<String, StatusCode> {
    parent_scope::require_user_path(provider, user_id, path.user_id.as_deref()).await?;
    Ok(path.id)
}
pub fn routes(provider: Arc<dyn MediaServerProvider>) -> Router {
    let state = AppState::new(provider.clone());

    let protected = Router::new()
        .route("/UserViews", get(user_views))
        .route("/Users/{user_id}/Views", get(user_views))
        .route("/Library/MediaFolders", get(media_folders))
        .route("/Library/VirtualFolders", get(virtual_folders))
        .route("/Users/{user_id}", get(user_profile))
        .route("/Users/Me", get(user_profile))
        .route("/Items/Counts", get(item_counts))
        .route("/Users/{user_id}/Items/Counts", get(item_counts))
        .route("/Items", get(items))
        .route("/socket", get(websocket::connect))
        .route("/Persons/{name}", get(people::person_by_name))
        .route("/Genres", get(genres))
        .route("/Genres/{name}", get(genre_by_name))
        .route("/Studios", get(studios))
        .route("/Studios/{name}", get(studio_by_name))
        .route("/Search/Hints", get(search_hints))
        .route("/Users/{user_id}/Items", get(items))
        .route("/Users/{user_id}/Items/Resume", get(resume_items))
        .route("/Users/{user_id}/Items/Latest", get(latest_items))
        .route("/Shows/NextUp", get(shows_next_up))
        .route("/Shows/{id}/Seasons", get(show_seasons))
        .route("/Shows/{id}/Episodes", get(show_episodes))
        .route("/Items/{id}", get(item_by_id))
        .route("/Users/{user_id}/Items/{id}", get(item_by_id))
        .route(
            "/Users/{user_id}/FavoriteItems/{id}",
            post(mark_favorite).delete(unmark_favorite),
        )
        .route(
            "/UserFavoriteItems/{id}",
            post(mark_favorite).delete(unmark_favorite),
        )
        .route(
            "/Users/{user_id}/PlayedItems/{id}",
            post(mark_played).delete(unmark_played),
        )
        .route(
            "/UserPlayedItems/{id}",
            post(mark_played).delete(unmark_played),
        )
        .route("/Items/{id}/PlaybackInfo", post(playback_info))
        .route("/QuickConnect/Enabled", get(quickconnect_enabled))
        .route(
            "/DisplayPreferences/{id}",
            get(display_preferences).post(update_display_preferences),
        )
        .route("/Sessions/Playing", post(session_playing))
        .route("/Sessions/Playing/Progress", post(session_progress))
        .route("/Sessions/Playing/Stopped", post(session_stopped))
        .route_layer(axum::middleware::from_fn(move |req, next| {
            let p = provider.clone();
            authenticate(p, req, next)
        }))
        .with_state(state.clone());

    let public = Router::new()
        .route("/Users/AuthenticateByName", post(authenticate_by_name))
        .route("/System/Info/Public", get(public_system_info))
        .route("/System/Ping", get(ping).post(ping))
        .route("/jellyfin", get(device_description))
        .with_state(state);

    let root = Router::new().merge(public).merge(protected);
    Router::new().merge(root.clone()).nest("/emby", root)
}

pub fn media_routes(provider: Arc<dyn MediaServerProvider>) -> Router {
    media::routes(provider)
}

// Handlers

fn parse_season_id(series_id: &str, season_id: &str) -> Option<u32> {
    let series_id = series_id.replace('-', "");
    let season_id = season_id.replace('-', "");
    let suffix = season_id.get(series_id.len()..)?;
    if !season_id[..series_id.len()].eq_ignore_ascii_case(&series_id) {
        return None;
    }
    suffix.parse().ok()
}
