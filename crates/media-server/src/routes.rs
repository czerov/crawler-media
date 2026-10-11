use axum::extract::{Extension, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::auth::{AuthUser, authenticate};
use crate::dto::{item_dto_json, library_view_json, user_profile_json};
use crate::provider::MediaServerProvider;
mod item_filters;
mod media;
mod people;
mod sessions;
mod websocket;
use item_filters::filter_items;
use media::playback_info;
use sessions::{session_playing, session_progress, session_stopped};

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

#[derive(Default, Deserialize)]
struct ViewsQuery {
    #[serde(default, rename = "IncludeHidden", alias = "includeHidden")]
    include_hidden: Option<bool>,
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
    if let Some(path_user) = path.user_id {
        let matches_id = path_user
            .replace('-', "")
            .eq_ignore_ascii_case(&user_id.to_string().replace('-', ""));
        if !matches_id && !path_user.eq_ignore_ascii_case("me") {
            let user = provider
                .get_user(user_id)
                .await
                .map_err(|error| {
                    tracing::error!(%error, %user_id, "failed to resolve Jellyfin user path");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?
                .ok_or(StatusCode::NOT_FOUND)?;
            if !user.login.eq_ignore_ascii_case(&path_user) {
                return Err(StatusCode::NOT_FOUND);
            }
        }
    }
    Ok(path.id)
}

#[derive(Deserialize)]
pub struct AuthBody {
    #[serde(alias = "Username")]
    pub username: String,
    #[serde(alias = "Pw")]
    pub pw: String,
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

async fn public_system_info(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "Id": state.provider.server_id(),
        "ServerName": "crawler-media",
        "ProductName": "crawler-media",
        "Version": "0.1.0",
        "StartupWizardCompleted": true,
    }))
}

async fn ping() -> Response {
    StatusCode::OK.into_response()
}

async fn device_description(State(state): State<AppState>) -> Response {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><root xmlns=\"urn:schemas-upnp-org:device-1-0\"><specVersion><major>1</major><minor>0</minor></specVersion><device><deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType><friendlyName>crawler-media</friendlyName><manufacturer>crawler-media</manufacturer><modelName>crawler-media</modelName><UDN>uuid:{}</UDN><presentationURL>/</presentationURL></device></root>",
        state.provider.server_id()
    );
    (
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

async fn authenticate_by_name(
    State(state): State<AppState>,
    Json(body): Json<AuthBody>,
) -> Result<Json<Value>, StatusCode> {
    let res = state
        .provider
        .authenticate_password(&body.username, &body.pw)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::UNAUTHORIZED)?;

    Ok(Json(json!({
        "AccessToken": res.0,
        "User": { "Id": res.1.id.to_string(), "Name": res.1.login },
    })))
}

async fn user_profile(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Value>, StatusCode> {
    let user_row = state
        .provider
        .get_user(user.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(user_profile_json(
        &user_row,
        &state.provider.server_id(),
    )))
}

async fn user_views(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ViewsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let libraries = state
        .provider
        .list_visible_libraries(user.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let server_id = state.provider.server_id();
    let include_hidden = query.include_hidden.unwrap_or(false);
    let views: Vec<Value> = libraries
        .iter()
        .filter(|lib| include_hidden || !lib.exclude_from_home)
        .map(|lib| library_view_json(lib, &server_id))
        .collect();
    let total = views.len();
    Ok(Json(json!({
        "Items": views,
        "TotalRecordCount": total,
    })))
}
async fn media_folders(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Value>, StatusCode> {
    let libraries = state
        .provider
        .list_visible_libraries(user.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let server_id = state.provider.server_id();
    let views: Vec<Value> = libraries
        .iter()
        .map(|lib| {
            let mut val = library_view_json(lib, &server_id);
            val["Type"] = json!("Folder");
            val["IsFolder"] = json!(true);
            val
        })
        .collect();
    let total = views.len();
    Ok(Json(json!({
        "Items": views,
        "TotalRecordCount": total,
    })))
}

async fn virtual_folders(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Value>, StatusCode> {
    let libraries = state
        .provider
        .list_visible_libraries(user.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let views: Vec<Value> = libraries
        .iter()
        .map(|lib| {
            json!({
                "Name": lib.name,
                "ItemId": lib.id,
                "Id": lib.id,
                "Locations": lib.root_paths,
                "CollectionType": match lib.kind {
                    domain::MediaKind::Movie => "movies",
                    domain::MediaKind::Tv => "tvshows",
                    domain::MediaKind::Video => "homevideos",
                },
            })
        })
        .collect();
    Ok(Json(json!(views)))
}

async fn item_counts(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Value>, StatusCode> {
    let items = state
        .provider
        .list_visible_items(user.id, None)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut movie_count = 0;
    let mut series_count = 0;
    for item in &items {
        if item.media.kind == domain::MediaKind::Movie {
            movie_count += 1;
        } else if item.is_series {
            series_count += 1;
        }
    }
    let mut episode_count = 0;
    for series in items.iter().filter(|item| item.is_series) {
        let series_id = series.media.id.to_string().replace('-', "");
        episode_count += state
            .provider
            .list_visible_items(user.id, Some(&series_id))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .len();
    }
    Ok(Json(json!({
        "MovieCount": movie_count,
        "SeriesCount": series_count,
        "EpisodeCount": episode_count,
        "ItemCount": items.len(),
    })))
}

async fn items(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(mut query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let libraries = state.provider.list_visible_libraries(user.id).await.unwrap_or_default();
    let is_folder_id = query.parent_id.as_deref().map(|pid| pid.starts_with("folder_")).unwrap_or(false);
    let is_library_parent = query.parent_id.as_deref().map(|pid| libraries.iter().any(|lib| lib.id == pid)).unwrap_or(false);
    let is_series_parent = query.series_id.is_some() || if let Some(pid) = query.parent_id.as_deref() {
        if is_library_parent || is_folder_id {
            false
        } else {
            state.provider.resolve_single_item(user.id, pid).await.ok().flatten().map(|it| it.is_series).unwrap_or(false)
        }
    } else {
        false
    };
    let parent_id_for_provider = if is_library_parent {
        query.parent_id.as_deref()
    } else if is_series_parent {
        query.parent_id.as_deref().or(query.series_id.as_deref())
    } else {
        None
    };
    let wants_episodes = query
        .include_item_types
        .as_deref()
        .map(|types| types.split(',').any(|t| t.trim().eq_ignore_ascii_case("Episode")))
        .unwrap_or(false);
    let is_folder_query = query.is_folder.unwrap_or(false);
    let recursive = query.recursive.unwrap_or(true);
    let mut rows = state
        .provider
        .list_visible_items(user.id, parent_id_for_provider)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if is_library_parent && (!recursive || is_folder_query) {
        if let Some(lib_id) = query.parent_id.as_deref() {
            if let Some(target_lib) = libraries.iter().find(|l| l.id == lib_id) {
                let mut folder_items: Vec<Value> = Vec::new();
                let server_id = state.provider.server_id();
                for root in &target_lib.root_paths {
                    let folder_name = root.file_name().and_then(|n| n.to_str()).unwrap_or("folder");
                    let folder_id = format!("folder_{}", uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, root.to_string_lossy().as_bytes()).simple());
                    folder_items.push(json!({
                        "Name": folder_name,
                        "ServerId": server_id,
                        "Id": folder_id,
                        "Type": "Folder",
                        "IsFolder": true,
                        "CollectionType": match target_lib.kind {
                            domain::MediaKind::Movie => "movies",
                            domain::MediaKind::Tv => "tvshows",
                            domain::MediaKind::Video => "homevideos",
                        },
                    }));
                }
                let total = folder_items.len();
                return Ok(Json(json!({ "Items": folder_items, "TotalRecordCount": total })));
            }
        }
    }
    if is_folder_id {
        if let Some(target_folder_id) = query.parent_id.take() {
            rows.retain(|item| {
                for lib in &libraries {
                    for root in &lib.root_paths {
                        let f_id = format!("folder_{}", uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, root.to_string_lossy().as_bytes()).simple());
                        if f_id == target_folder_id {
                            return std::path::Path::new(&item.row.path).starts_with(root);
                        }
                    }
                }
                false
            });
        }
    }
    if wants_episodes && rows.iter().any(|r| r.is_series) {
        let mut expanded = Vec::new();
        for item in rows {
            if item.is_series {
                let series_id = item.media.id.to_string().replace('-', "");
                if let Ok(episodes) = state.provider.list_visible_items(user.id, Some(&series_id)).await {
                    expanded.extend(episodes);
                }
            } else {
                expanded.push(item);
            }
        }
        rows = expanded;
    }
    let mut query = query;
    let is_genre_or_studio = !is_library_parent && query.parent_id.as_deref().map(|pid| {
        rows.iter().any(|item| {
            item.metadata.genres.iter().any(|g| {
                let u = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, g.as_bytes()).hyphenated().to_string();
                let s = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, g.as_bytes()).simple().to_string();
                pid.eq_ignore_ascii_case(&u) || pid.eq_ignore_ascii_case(&s) || g.eq_ignore_ascii_case(pid)
            }) || item.metadata.studios.iter().any(|st| {
                let u = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, st.as_bytes()).hyphenated().to_string();
                let s = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, st.as_bytes()).simple().to_string();
                pid.eq_ignore_ascii_case(&u) || pid.eq_ignore_ascii_case(&s) || st.eq_ignore_ascii_case(pid)
            })
        })
    }).unwrap_or(false);
    if is_genre_or_studio {
        if let Some(target_id) = query.parent_id.take() {
            rows.retain(|item| {
                item.metadata.genres.iter().any(|g| {
                    let u = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, g.as_bytes()).hyphenated().to_string();
                    let s = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, g.as_bytes()).simple().to_string();
                    target_id.eq_ignore_ascii_case(&u) || target_id.eq_ignore_ascii_case(&s) || g.eq_ignore_ascii_case(&target_id)
                }) || item.metadata.studios.iter().any(|st| {
                    let u = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, st.as_bytes()).hyphenated().to_string();
                    let s = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, st.as_bytes()).simple().to_string();
                    target_id.eq_ignore_ascii_case(&u) || target_id.eq_ignore_ascii_case(&s) || st.eq_ignore_ascii_case(&target_id)
                })
            });
        }
    }
    if query.person_ids.is_some() {
        let mut enriched = Vec::with_capacity(rows.len());
        for row in rows {
            enriched.push(state.provider.enrich_visible_item(row).await);
        }
        rows = enriched;
    }
    let rows = filter_items(rows, &query);
    let episodes_only = !rows.is_empty()
        && rows
            .iter()
            .all(|item| item.media.kind == domain::MediaKind::Tv && !item.is_series);
    let (default_sort, default_order) = if episodes_only {
        ("ParentIndexNumber,IndexNumber", "Ascending")
    } else {
        ("DateCreated,SortName", "Descending")
    };
    let rows = item_filters::sort_items(
        rows,
        query.sort_by.as_deref().unwrap_or(default_sort),
        query.sort_order.as_deref().unwrap_or(default_order),
    );
    let total = rows.len();
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    let listed: Vec<Value> = rows
        .into_iter()
        .skip(start_index)
        .take(limit)
        .map(item_dto_json)
        .collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": total })))
}

async fn resume_items(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let root_rows = state
        .provider
        .list_visible_items(user.id, query.parent_id.as_deref())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut rows = Vec::new();
    for item in root_rows {
        if item.media.kind == domain::MediaKind::Tv && item.is_series {
            let series_id = item.media.id.to_string().replace('-', "");
            let mut episodes = state
                .provider
                .list_visible_items(user.id, Some(&series_id))
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            episodes.retain(|episode| episode.ticks > 0 && !episode.played);
            episodes.sort_by_key(|episode| {
                (
                    episode.metadata.resume_updated_at,
                    episode.row.season.unwrap_or_default(),
                    episode.row.episode.unwrap_or_default(),
                )
            });
            if let Some(episode) = episodes.pop() {
                rows.push(episode);
            }
        } else if item.ticks > 0 && !item.played {
            rows.push(item);
        }
    }
    let total = rows.len();
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    let listed: Vec<Value> = rows
        .drain(..)
        .skip(start_index)
        .take(limit)
        .map(item_dto_json)
        .collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": total })))
}

async fn latest_items(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let rows = state
        .provider
        .list_visible_items(user.id, query.parent_id.as_deref())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    // 「最近入库」的播放状态口径（见 library::latest）：
    // 显式给了 IsPlayed 就按它严格筛（协议语义原样保留）；没给时**只排不筛**——
    // 没看完的在前、已看完的沉底，段内仍是入库时间倒序。
    // Jellyfin 在缺省位置会因为 HidePlayedInLatest 直接把已看完的藏掉，全看过的库
    // 更是连入口都没有；我们改成排到最后，客户端那一行于是永远有内容。
    let rows = item_filters::filter_items_except_played(rows, &query);
    let rows = item_filters::sort_items(rows, "DateCreated,SortName", "Descending");
    let rows = match query.is_played {
        Some(played) => rows
            .into_iter()
            .filter(|item| item.played == played)
            .collect(),
        None => library::played_last(rows, |item| item.played),
    };
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let listed: Vec<Value> = rows
        .into_iter()
        .skip(start_index)
        .take(limit)
        .map(item_dto_json)
        .collect();
    Ok(Json(json!(listed)))
}
async fn genres(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let libraries = state.provider.list_visible_libraries(user.id).await.unwrap_or_default();
    let is_library_parent = query.parent_id.as_deref().map(|pid| libraries.iter().any(|lib| lib.id == pid)).unwrap_or(false);
    let parent_id = if is_library_parent { query.parent_id.as_deref() } else { None };
    let items = state
        .provider
        .list_visible_items(user.id, parent_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut genre_counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for it in items {
        for g in it.metadata.genres {
            if !g.trim().is_empty() {
                *genre_counts.entry(g).or_insert(0) += 1;
            }
        }
    }
    let server_id = state.provider.server_id();
    let search = query.search_term.as_deref().map(|s| s.to_lowercase());
    let mut dtos = Vec::new();
    for (name, count) in genre_counts {
        if let Some(term) = &search {
            if !name.to_lowercase().contains(term) {
                continue;
            }
        }
        dtos.push(json!({
            "Name": name,
            "ServerId": server_id,
            "Id": uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes()).hyphenated().to_string(),
            "Type": "Genre",
            "IsFolder": true,
            "ChildCount": count,
            "ImageTags": { "Primary": "genre" },
            "BackdropImageTags": ["genre"],
        }));
    }
    let total = dtos.len();
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    let listed: Vec<Value> = dtos.into_iter().skip(start_index).take(limit).collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": total })))
}

async fn genre_by_name(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let server_id = state.provider.server_id();
    Ok(Json(json!({
        "Name": name,
        "ServerId": server_id,
        "Id": uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes()).hyphenated().to_string(),
        "Type": "Genre",
        "IsFolder": true,
        "ImageTags": { "Primary": "genre" },
        "BackdropImageTags": ["genre"],
    })))
}

async fn studios(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let libraries = state.provider.list_visible_libraries(user.id).await.unwrap_or_default();
    let is_library_parent = query.parent_id.as_deref().map(|pid| libraries.iter().any(|lib| lib.id == pid)).unwrap_or(false);
    let parent_id = if is_library_parent { query.parent_id.as_deref() } else { None };
    let items = state
        .provider
        .list_visible_items(user.id, parent_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut studio_counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for it in items {
        for s in it.metadata.studios {
            if !s.trim().is_empty() {
                *studio_counts.entry(s).or_insert(0) += 1;
            }
        }
    }
    let server_id = state.provider.server_id();
    let search = query.search_term.as_deref().map(|s| s.to_lowercase());
    let mut dtos = Vec::new();
    for (name, count) in studio_counts {
        if let Some(term) = &search {
            if !name.to_lowercase().contains(term) {
                continue;
            }
        }
        dtos.push(json!({
            "Name": name,
            "ServerId": server_id,
            "Id": uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes()).hyphenated().to_string(),
            "Type": "Studio",
            "IsFolder": true,
            "ChildCount": count,
            "ImageTags": { "Primary": "studio" },
            "BackdropImageTags": ["studio"],
        }));
    }
    let total = dtos.len();
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    let listed: Vec<Value> = dtos.into_iter().skip(start_index).take(limit).collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": total })))
}

async fn studio_by_name(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let server_id = state.provider.server_id();
    Ok(Json(json!({
        "Name": name,
        "ServerId": server_id,
        "Id": uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes()).hyphenated().to_string(),
        "Type": "Studio",
        "IsFolder": true,
        "ImageTags": { "Primary": "studio" },
        "BackdropImageTags": ["studio"],
    })))
}

#[derive(Default, Deserialize)]
struct SearchHintsQuery {
    #[serde(default, rename = "SearchTerm", alias = "searchTerm")]
    search_term: Option<String>,
    #[serde(default, rename = "StartIndex", alias = "startIndex")]
    start_index: Option<usize>,
    #[serde(default, rename = "Limit", alias = "limit")]
    limit: Option<usize>,
}

async fn search_hints(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<SearchHintsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let Some(term) = query.search_term.map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()) else {
        return Ok(Json(json!({ "SearchHints": [], "TotalRecordCount": 0 })));
    };
    let items = state
        .provider
        .list_visible_items(user.id, None)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut hints = Vec::new();
    for it in items {
        let title_match = it.media.title.to_lowercase().contains(&term);
        let orig_match = it.metadata.original_title.as_deref().map(|s| s.to_lowercase().contains(&term)).unwrap_or(false);
        if title_match || orig_match {
            let id = if it.is_series {
                it.media.id.to_string().replace('-', "")
            } else {
                it.row.id.to_string().replace('-', "")
            };
            let item_type = if it.is_series {
                "Series"
            } else if it.media.kind == domain::MediaKind::Tv {
                "Episode"
            } else {
                "Movie"
            };
            hints.push(json!({
                "ItemId": id,
                "Id": id,
                "Name": it.media.title,
                "Type": item_type,
                "IsFolder": it.is_series,
                "RunTimeTicks": it.metadata.runtime_ticks,
                "ProductionYear": it.metadata.production_year,
                "MatchedTerm": term,
            }));
        }
    }
    let total = hints.len();
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let listed: Vec<Value> = hints.into_iter().skip(start_index).take(limit).collect();
    Ok(Json(json!({ "SearchHints": listed, "TotalRecordCount": total })))
}

async fn shows_next_up(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let series = state
        .provider
        .list_visible_items(user.id, None)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let target_series_id = query.series_id.as_deref().map(|s| s.replace('-', ""));
    let mut next_up = Vec::new();
    for show in series
        .iter()
        .filter(|item| item.media.kind == domain::MediaKind::Tv && item.is_series)
    {
        let series_id = show.media.id.to_string().replace('-', "");
        if let Some(target) = &target_series_id {
            if !series_id.eq_ignore_ascii_case(target) {
                continue;
            }
        }
        let mut episodes = state
            .provider
            .list_visible_items(user.id, Some(&series_id))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        episodes.sort_by_key(|item| {
            (
                item.row.season.unwrap_or_default(),
                item.row.episode.unwrap_or_default(),
            )
        });
        if let Some(episode) = episodes.into_iter().find(|item| !item.played) {
            next_up.push(item_dto_json(episode));
        }
    }
    let total = next_up.len();
    let start_index = query.start_index.unwrap_or(0);
    let listed: Vec<Value> = next_up.into_iter().skip(start_index).take(limit).collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": total })))
}

async fn show_seasons(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let item = state
        .provider
        .resolve_single_item(user.id, &id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let compact_id = item.media.id.to_string().replace('-', "");
    let episodes = state
        .provider
        .list_visible_items(user.id, Some(&compact_id))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut season_numbers: std::collections::BTreeSet<u32> = episodes
        .into_iter()
        .map(|episode| episode.row.season.unwrap_or(1))
        .collect();
    if season_numbers.is_empty() {
        season_numbers.insert(item.row.season.unwrap_or(1));
    }
    let server_id = state.provider.server_id();
    let mut seasons_json: Vec<Value> = Vec::new();
    for season_num in season_numbers {
        let season_id = format!("{compact_id}{season_num:02}");
        let user_data = state
            .provider
            .resolve_user_item_data(user.id, &season_id)
            .await
            .ok()
            .flatten()
            .map(|data| data.json_value())
            .unwrap_or_else(|| {
                json!({
                    "PlaybackPositionTicks": 0,
                    "PlayCount": 0,
                    "IsFavorite": false,
                    "Played": false,
                })
            });
        let primary_tag = item.metadata.primary_image_tag.clone();
        seasons_json.push(json!({
            "Id": season_id,
            "Name": format!("第 {} 季", season_num),
            "SeriesId": compact_id,
            "SeriesName": item.media.title,
            "IndexNumber": season_num,
            "Type": "Season",
            "IsFolder": true,
            "ServerId": server_id,
            "UserData": user_data,
            "SeriesPrimaryImageTag": primary_tag,
            "ImageTags": {
                "Primary": primary_tag.unwrap_or_else(|| "poster".to_string())
            },
            "BackdropImageTags": [],
        }));
    }
    Ok(Json(
        json!({ "Items": seasons_json, "TotalRecordCount": seasons_json.len() }),
    ))
}

async fn show_episodes(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let compact_id = id.replace('-', "");
    let rows = state
        .provider
        .list_visible_items(user.id, Some(&compact_id))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let season_number = query
        .season_id
        .as_deref()
        .and_then(|season_id| parse_season_id(&compact_id, season_id));
    let limit = query.limit.unwrap_or(200);
    let mut rows: Vec<_> = rows
        .into_iter()
        .filter(|episode| match season_number {
            Some(season) => episode.row.season == Some(season),
            None => query.season_id.is_none(),
        })
        .collect();
    rows.sort_by_key(|episode| {
        (
            episode.row.season.unwrap_or_default(),
            episode.row.episode.unwrap_or_default(),
        )
    });
    let total = rows.len();
    let listed: Vec<Value> = rows
        .into_iter()
        .skip(query.start_index.unwrap_or(0))
        .take(limit)
        .map(item_dto_json)
        .collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": total })))
}

fn parse_season_id(series_id: &str, season_id: &str) -> Option<u32> {
    let series_id = series_id.replace('-', "");
    let season_id = season_id.replace('-', "");
    let suffix = season_id.get(series_id.len()..)?;
    if !season_id[..series_id.len()].eq_ignore_ascii_case(&series_id) {
        return None;
    }
    suffix.parse().ok()
}

async fn item_by_id(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserItemPath>,
) -> Result<Json<Value>, StatusCode> {
    let id = item_id_for_user(state.provider.as_ref(), path, user.id).await?;
    let snapshot = state
        .provider
        .resolve_single_item(user.id, &id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(item_dto_json(snapshot)))
}

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

async fn mark_favorite(
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

async fn unmark_favorite(
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

async fn mark_played(
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

async fn unmark_played(
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

async fn quickconnect_enabled() -> Json<Value> {
    Json(json!(false))
}

async fn display_preferences(Path(id): Path<String>) -> Json<Value> {
    Json(json!({
        "Id": id,
        "SortBy": "SortName",
        "RememberIndexing": false,
        "PrimaryImageHeight": 250,
        "PrimaryImageWidth": 250,
        "CustomPrefs": {},
        "ScrollDirection": "Horizontal",
        "ShowBackdrop": true,
        "RememberSorting": false,
        "SortOrder": "Ascending",
        "ShowSidebar": false,
        "Client": "emby"
    }))
}

async fn update_display_preferences() -> Response {
    StatusCode::NO_CONTENT.into_response()
}
