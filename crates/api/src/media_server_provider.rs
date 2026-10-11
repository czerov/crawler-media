use async_trait::async_trait;
use domain::{MediaKind, UserId};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use media_server::provider::{
    MediaItemSnapshot, MediaPerson, MediaServerProvider, PlaybackClientInfo, PlaybackEvent,
    ServerLibrary, ServerUser,
};

use crate::management::ApiState;
mod item_metadata;
mod person_images;
mod playback;
use item_metadata::{
    backdrop_path, item_metadata, local_primary_image, merge_catalog_metadata, primary_image_url,
};

mod enrichment;
mod items;
mod user_marks;
use items::resolve_fallback_item;
use user_marks::{persist_unplayed_override, resolve_mark_unit, write_favorite_mark};

pub(crate) fn default_library_cover(kind: MediaKind) -> Result<Vec<u8>, String> {
    const DEFAULT_MOVIE_COVER: &[u8] = include_bytes!("../assets/covers/movie.jpg");
    const DEFAULT_TV_COVER: &[u8] = include_bytes!("../assets/covers/tv.jpg");
    const DEFAULT_VIDEO_COVER: &[u8] = include_bytes!("../assets/covers/video.jpg");

    let bytes = match kind {
        MediaKind::Movie => DEFAULT_MOVIE_COVER,
        MediaKind::Tv => DEFAULT_TV_COVER,
        MediaKind::Video => DEFAULT_VIDEO_COVER,
    };
    Ok(bytes.to_vec())
}

fn library_cover_tag(cover_path: Option<&std::path::Path>, kind: MediaKind) -> String {
    let bytes = match cover_path {
        Some(path) => match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(%error, cover_path = %path.display(), "failed to read library cover for cache tag");
                default_library_cover(kind).unwrap_or_default()
            }
        },
        None => default_library_cover(kind).unwrap_or_default(),
    };
    format!("cover-{:x}", Sha256::digest(bytes))
}

fn is_series_item_id(id: &str, media_id: domain::MediaId) -> bool {
    id.parse::<domain::MediaId>()
        .is_ok_and(|requested_media_id| requested_media_id == media_id)
}

fn catalog_episode_still_url(path: &str) -> Option<String> {
    if path.starts_with("https://image.tmdb.org/") || path.starts_with("http://image.tmdb.org/") {
        Some(path.to_string())
    } else if path.starts_with("/t/p/") {
        Some(format!("https://image.tmdb.org{path}"))
    } else if path.starts_with('/') {
        Some(format!("https://image.tmdb.org/t/p/w300{path}"))
    } else {
        None
    }
}

async fn resolve_catalog_episode_still(
    catalog: std::sync::Arc<dyn crate::catalog::Catalog>,
    tmdb_id: String,
    season: u32,
    episode: u32,
) -> Option<String> {
    let lookup_id = tmdb_id.clone();
    match tokio::task::spawn_blocking(move || catalog.episode_stills(&lookup_id, season, episode))
        .await
    {
        Ok(Ok(paths)) => paths
            .first()
            .and_then(|path| catalog_episode_still_url(path)),
        Ok(Err(error)) => {
            tracing::warn!(%error, tmdb_id, season, episode, "failed to resolve Jellyfin episode still from catalog");
            None
        }
        Err(error) => {
            tracing::warn!(%error, tmdb_id, season, episode, "Jellyfin episode still lookup task failed");
            None
        }
    }
}
pub struct ApiServerProvider {
    pub state: ApiState,
}

impl ApiServerProvider {
    pub fn new(state: ApiState) -> Self {
        Self { state }
    }
}

#[async_trait]
impl MediaServerProvider for ApiServerProvider {
    fn server_id(&self) -> String {
        self.state.jellyfin_server_id().to_string()
    }

    async fn authenticate_password(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<(String, ServerUser)>, String> {
        let store = self.state.store.lock();
        let token_opt = store
            .verify_user_password(username, password)
            .map_err(|e| e.to_string())?;
        let Some(token) = token_opt else {
            return Ok(None);
        };
        let Some(user) = store.user_by_token(&token).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        let role = store.user_role(user.id).unwrap_or_else(|_| "member".into());
        Ok(Some((
            token,
            ServerUser {
                id: user.id,
                login: user.login,
                is_admin: role == "admin",
            },
        )))
    }

    async fn user_id_by_token(&self, token: &str) -> Result<Option<UserId>, String> {
        let store = self.state.store.lock();
        store.user_id_by_token(token).map_err(|e| e.to_string())
    }

    async fn bind_playback_credential(
        &self,
        user_id: UserId,
        device_id: &str,
        token: &str,
    ) -> Result<(), String> {
        self.state
            .store
            .lock()
            .bind_playback_credential(user_id, device_id, token)
            .map_err(|error| error.to_string())
    }

    async fn get_user(&self, user_id: UserId) -> Result<Option<ServerUser>, String> {
        let store = self.state.store.lock();
        let Some(user) = store.get_user(user_id).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        let role = store.user_role(user.id).unwrap_or_else(|_| "member".into());
        Ok(Some(ServerUser {
            id: user.id,
            login: user.login,
            is_admin: role == "admin",
        }))
    }

    async fn list_visible_libraries(&self, user_id: UserId) -> Result<Vec<ServerLibrary>, String> {
        let store = self.state.store.lock();
        let libraries = store.list_libraries().map_err(|e| e.to_string())?;
        let mut list = Vec::new();
        for lib in libraries {
            if !crate::http::library::library_visible(&store, &lib, Some(user_id)) {
                continue;
            }
            let cover_path = crate::http::library::library_cover_path(&store, &lib);
            let cover_tag = library_cover_tag(cover_path.as_deref(), lib.kind);
            list.push(ServerLibrary {
                id: lib.id,
                name: lib.name,
                kind: lib.kind,
                exclude_from_home: lib.exclude_from_home,
                root_paths: lib.root_paths,
                cover_path: cover_path.map(|path| path.display().to_string()),
                cover_tag,
            });
        }
        Ok(list)
    }

    async fn list_visible_items(
        &self,
        user_id: UserId,
        parent_id: Option<&str>,
    ) -> Result<Vec<MediaItemSnapshot>, String> {
        items::list_visible_items(self, user_id, parent_id)
    }

    async fn resolve_single_item(
        &self,
        user_id: UserId,
        id: &str,
    ) -> Result<Option<MediaItemSnapshot>, String> {
        let items = self.list_visible_items(user_id, None).await?;
        let compact_id = id.replace('-', "");
        for it in items {
            let target = if it.is_series {
                it.media.id.to_string().replace('-', "")
            } else {
                it.row.id.to_string().replace('-', "")
            };
            if compact_id == target || id == target || id == it.media.id.to_string() {
                return Ok(Some(self.enrich_single_item(it).await));
            }
        }

        let fallback = resolve_fallback_item(&self.state, user_id, id, &compact_id);
        if let Some(snapshot) = fallback {
            return Ok(Some(self.enrich_single_item(snapshot).await));
        }
        Ok(None)
    }

    async fn resolve_user_item_data(
        &self,
        user_id: UserId,
        id: &str,
    ) -> Result<Option<media_server::provider::UserItemData>, String> {
        let compact_id = id.replace('-', "");
        let items = self.list_visible_items(user_id, None).await?;
        for it in items {
            let target = if it.is_series {
                it.media.id.to_string().replace('-', "")
            } else {
                it.row.id.to_string().replace('-', "")
            };
            if compact_id == target || id == target || id == it.media.id.to_string() {
                return Ok(Some(it.user_item_data()));
            }
        }
        if let Some(snapshot) = resolve_fallback_item(&self.state, user_id, id, &compact_id) {
            return Ok(Some(snapshot.user_item_data()));
        }
        let season_target = {
            let store = self.state.store.lock();
            crate::http::media_visibility::resolve_visible_season(&store, id, Some(user_id))
        };
        let Some((row, season)) = season_target else {
            return Ok(None);
        };
        let series_id = row.media_id.to_string().replace('-', "");
        let episodes = self
            .list_visible_items(user_id, Some(&series_id))
            .await?
            .into_iter()
            .filter(|episode| episode.row.season == Some(season))
            .collect::<Vec<_>>();
        let total = episodes.len();
        let played = episodes.iter().filter(|episode| episode.played).count();
        let favorite = {
            let store = self.state.store.lock();
            store
                .unit_state(
                    user_id,
                    row.media_id,
                    season as i32,
                    crate::store::UNIT_WHOLE,
                )
                .map_err(|error| error.to_string())?
                .is_some_and(|state| state.favorite)
        };
        let unplayed_item_count = total.saturating_sub(played);
        let item_id = id.replace('-', "");
        Ok(Some(media_server::provider::UserItemData {
            key: item_id.clone(),
            item_id,
            playback_position_ticks: 0,
            play_count: 0,
            played: total == 0 || unplayed_item_count == 0,
            is_favorite: favorite,
            unplayed_item_count: Some(unplayed_item_count),
            played_percentage: (total > 0).then(|| played as f64 / total as f64 * 100.0),
        }))
    }

    async fn enrich_visible_item(&self, item: MediaItemSnapshot) -> MediaItemSnapshot {
        self.enrich_single_item(item).await
    }

    async fn resolve_cover_bytes(
        &self,
        user_id: Option<UserId>,
        id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let store = self.state.store.lock();
        if let Ok(Some(lib)) = store.get_library(id) {
            if crate::http::library::library_visible(&store, &lib, user_id) {
                if let Some(cover) = crate::http::library::library_cover_path(&store, &lib) {
                    drop(store);
                    return std::fs::read(cover).map(Some).map_err(|e| e.to_string());
                }
                let kind = lib.kind;
                drop(store);
                return default_library_cover(kind).map(Some);
            }
        }
        Ok(None)
    }

    async fn resolve_poster_bytes(
        &self,
        user_id: Option<UserId>,
        id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let store = self.state.store.lock();
        if let Some(row) = crate::http::media_visibility::resolve_visible_row(&store, id, user_id) {
            let media = store.get_media(row.media_id).ok().flatten();
            let path = if is_series_item_id(id, row.media_id) {
                crate::http::library::poster_path(&row)
            } else {
                media
                    .as_ref()
                    .and_then(|media| local_primary_image(&row, media))
                    .or_else(|| crate::http::library::poster_path(&row))
            };
            if let Some(path) = path {
                drop(store);
                return std::fs::read(path).map(Some).map_err(|e| e.to_string());
            }
        }
        Ok(None)
    }

    async fn resolve_primary_image_url(
        &self,
        user_id: Option<UserId>,
        id: &str,
    ) -> Result<Option<String>, String> {
        let (nfo_url, local_still, tmdb_episode) = {
            let store = self.state.store.lock();
            let Some(row) = crate::http::media_visibility::resolve_visible_row(&store, id, user_id)
            else {
                return Ok(None);
            };
            let Some(media) = store.get_media(row.media_id).ok().flatten() else {
                return Ok(None);
            };
            if media.kind != MediaKind::Tv || is_series_item_id(id, row.media_id) {
                return Ok(None);
            }
            let Some((season, episode)) = row.season.zip(row.episode) else {
                return Ok(None);
            };
            let nfo_url = primary_image_url(&row, &media);
            let local_still = crate::episode_still::existing(std::path::Path::new(&row.path)).is_some();
            let tmdb_episode = media
                .tmdb_id
                .filter(|_| !local_still && nfo_url.is_none())
                .map(|tmdb_id| (tmdb_id, season, episode));
            (nfo_url, local_still, tmdb_episode)
        };
        if local_still {
            return Ok(None);
        }
        if nfo_url.is_some() {
            return Ok(nfo_url);
        }
        if let Some((tmdb_id, season, episode)) = tmdb_episode {
            return Ok(resolve_catalog_episode_still(
                self.state.catalog.clone(),
                tmdb_id,
                season,
                episode,
            )
            .await);
        }
        Ok(None)
    }

    async fn resolve_person_image_url(
        &self,
        user_id: Option<domain::UserId>,
        name: &str,
    ) -> Result<Option<String>, String> {
        person_images::resolve_person_image_url(self, user_id, name).await
    }

    async fn resolve_person(
        &self,
        user_id: UserId,
        name: &str,
    ) -> Result<Option<MediaPerson>, String> {
        person_images::resolve_person(self, user_id, name).await
    }

    async fn resolve_backdrop_bytes(
        &self,
        user_id: Option<UserId>,
        id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let store = self.state.store.lock();
        let Some(row) = crate::http::media_visibility::resolve_visible_row(&store, id, user_id)
        else {
            return Ok(None);
        };
        let is_series = id
            .replace('-', "")
            .eq_ignore_ascii_case(&row.media_id.to_string().replace('-', ""));
        let Some(path) = backdrop_path(&row, is_series) else {
            return Ok(None);
        };
        drop(store);
        std::fs::read(path)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    async fn resolve_stream_source(
        &self,
        user_id: UserId,
        id: &str,
    ) -> Result<Option<(PathBuf, bool)>, String> {
        let store = self.state.store.lock();
        if let Some(row) =
            crate::http::media_visibility::resolve_visible_row(&store, id, Some(user_id))
        {
            let path = PathBuf::from(&row.path);
            let is_strm = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("strm"));
            return Ok(Some((path, is_strm)));
        }
        Ok(None)
    }

    async fn is_device_revoked(&self, user_id: UserId, device_id: &str) -> Result<bool, String> {
        let store = self.state.store.lock();
        store
            .is_device_revoked(user_id, device_id)
            .map_err(|e| e.to_string())
    }

    async fn set_user_marks(
        &self,
        user_id: UserId,
        id: &str,
        played: Option<bool>,
        favorite: Option<bool>,
    ) -> Result<bool, String> {
        let store = self.state.store.lock();
        let Some((media_id, season, episode)) = resolve_mark_unit(&store, user_id, id) else {
            return Ok(false);
        };
        let now = crate::job_loop::unix_now();
        if played.is_none() {
            let Some(favorite) = favorite else {
                return Ok(true);
            };
            write_favorite_mark(&store, user_id, media_id, season, episode, favorite, now)?;
            if !favorite
                && season == crate::store::UNIT_WHOLE
                && episode == crate::store::UNIT_WHOLE
            {
                let _ = store.set_unit_marks(
                    user_id,
                    media_id,
                    season,
                    episode,
                    None,
                    Some(false),
                    now,
                );
            }
            return Ok(true);
        }
        store
            .set_unit_marks(user_id, media_id, season, episode, played, favorite, now)
            .map_err(|error| {
                tracing::error!(%error, %user_id, %media_id, season, episode, "failed to update Jellyfin user marks");
                error.to_string()
            })?;
        if let Some(false) = played {
            persist_unplayed_override(&store, user_id, media_id, season, episode, favorite, now)?;
        }
        Ok(true)
    }

    async fn update_progress(
        &self,
        user_id: UserId,
        id: &str,
        position_ms: i64,
        _paused: bool,
    ) -> Result<(), String> {
        playback::update_progress(self, user_id, id, position_ms)
    }

    async fn report_playback_event(
        &self,
        user_id: UserId,
        id: &str,
        position_ms: i64,
        paused: bool,
        client: PlaybackClientInfo,
        event: PlaybackEvent,
    ) -> Result<(), String> {
        playback::report_event(self, user_id, id, position_ms, paused, client, event)
    }
}
