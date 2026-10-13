//! Library browse: Library listing and ledger-grouped item views.
//! Entity CRUD lives in `library_config`, scans/refresh in `library_scan`,
//! artwork in `library_artwork`, admin views in `library_admin`.

mod probe;
pub(crate) use probe::{
    enqueue_probes_for_paths, enqueue_probes_for_rows, ensure_probe_enqueued, file_tracks,
    probe_item,
};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{MediaId, MediaKind};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::str::FromStr;

use crate::catalog::Catalog;
use crate::http::{err, ok, ok_list};
use crate::management::ApiState;
use crate::store::Library;

mod assets;
mod cover;
pub(crate) mod facets;
pub(crate) mod page;
pub(crate) mod selection;
pub(crate) use assets::{library_json, poster_path, preferred_row, rows_in_library};
pub(crate) use cover::{default_cover_options, is_legacy_fanart_copy, render_cover_jpeg};

/// Resolve one ledger row to exactly one Library. The longest matching root
/// wins, so a nested Library does not leak through a broader parent root. Two
/// Libraries with equally-specific roots are ambiguous and therefore own
/// nothing until the configuration is fixed.
pub(crate) fn library_for_row(
    store: &crate::Store,
    row: &domain::LedgerRow,
    media: &domain::Media,
) -> Option<Library> {
    store
        .library_for_path(FsPath::new(&row.path), media.kind)
        .ok()
        .flatten()
}

/// Destructive file operations must not fall back to the default Library.
pub(crate) fn library_for_row_strict(
    store: &crate::Store,
    row: &domain::LedgerRow,
    media: &domain::Media,
) -> Option<Library> {
    store
        .library_for_path_strict(FsPath::new(&row.path), media.kind)
        .ok()
        .flatten()
}

pub(crate) fn row_visible_to_user(
    store: &crate::Store,
    row: &domain::LedgerRow,
    media: &domain::Media,
    user_id: Option<domain::UserId>,
) -> bool {
    library_for_row(store, row, media)
        .is_some_and(|library| library_visible(store, &library, user_id))
}

/// Whether a library is visible to the given user: admins see everything
/// (subject to the library's admin_visible flag for browse scope); members see
/// "everyone" libraries plus "selected" ones that list them.
pub(crate) fn library_visible(
    store: &crate::Store,
    library: &Library,
    user_id: Option<domain::UserId>,
) -> bool {
    if !store.library_accepts_work(&library.id).unwrap_or(false) {
        return false;
    }
    let Some(user_id) = user_id else {
        return true;
    };
    if store
        .user_role(user_id)
        .as_deref()
        .map(|r| r == "admin")
        .unwrap_or(false)
    {
        return library.admin_visible;
    }
    if library.access_mode == "everyone" {
        return true;
    }
    library.member_ids.contains(&user_id)
}

pub(crate) fn missing_library() -> Response {
    err(StatusCode::NOT_FOUND, "library.missing", "媒体库不存在")
}

pub(crate) fn artwork_url(kind: &str, ledger_id: domain::LedgerId) -> String {
    format!("/{kind}/{ledger_id}")
}

pub(crate) fn require_visible_library(
    store: &crate::Store,
    id: &str,
    user_id: Option<domain::UserId>,
) -> Result<Library, Response> {
    let Some(library) = store.get_library(id).ok().flatten() else {
        return Err(missing_library());
    };
    if !library_visible(store, &library, user_id) {
        return Err(missing_library());
    }
    Ok(library)
}

/// 长图（fanart.jpg）向上查找：与 poster_path 同一套布局兼容。
pub(crate) fn backdrop_path(row: &domain::LedgerRow) -> Option<PathBuf> {
    let file_dir = FsPath::new(&row.path).parent()?;
    for dir in [file_dir, file_dir.parent().unwrap_or(file_dir)] {
        let art = dir.join("fanart.jpg");
        if art.is_file() {
            return Some(art);
        }
    }
    None
}

/// 解析媒体库封面：**只读**已存在的版本文件，不生成、不写盘、不写 DB。
/// 自动生成统一走 `LibraryCoverService::ensure_cover`。
pub(crate) fn library_cover_path(store: &crate::Store, library: &Library) -> Option<PathBuf> {
    // 0. 用户显式删除（"none"/空串）时不回填
    match library.cover_path.as_deref() {
        Some("none") | Some("") => return None,
        Some(path) => {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Some(path);
            }
        }
        None => {}
    }
    // 1. legacy 固定路径 `library-{id}.jpg` 仅读、不 unlink
    let legacy = store
        .library_covers_dir()
        .join(format!("library-{}.jpg", library.id));
    legacy.is_file().then_some(legacy)
}

fn library_cover_checked_key(library_id: &str) -> String {
    format!("library_cover_checked.{library_id}")
}

pub(crate) fn clear_library_cover_checked(store: &crate::Store, library_id: &str) {
    let key = library_cover_checked_key(library_id);
    match store.get_setting(&key) {
        Ok(Some(value)) if value == "auto-v2" => {
            if let Err(error) = store.delete_setting(&key) {
                tracing::warn!(%error, library_id, "failed to clear library cover check marker");
            }
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(%error, library_id, "failed to read library cover check marker");
        }
    }
}

/// GET /libraries?kind= — one entry per Library entity, defaults first;
/// members only see libraries visible to them.
pub(crate) async fn list_libraries(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let store = state.store.lock();
    let libraries = match store.list_libraries() {
        Ok(libraries) => libraries,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let is_admin = user_id
        .and_then(|uid| store.user_role(uid).ok())
        .as_deref()
        .map(|r| r == "admin")
        .unwrap_or(false);
    let manage_scope = query.get("scope").is_some_and(|s| s == "admin")
        || query.get("manage").is_some_and(|s| s == "1" || s == "true");
    let libraries: Vec<Library> = libraries
        .into_iter()
        .filter(|library| {
            if is_admin && manage_scope {
                true
            } else {
                library_visible(&store, library, user_id)
            }
        })
        .collect();
    let kind = query.get("kind").and_then(|k| MediaKind::from_str(k).ok());
    let items: Vec<Value> = libraries
        .iter()
        .filter(|library| kind.is_none_or(|k| library.kind == k))
        .map(|library| library_json(&store, library))
        .collect();
    ok_list(items).into_response()
}

/// GET /libraries/{id}/items — one item per Media owned under this library's roots.
pub(crate) async fn list_items(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    // 显式分页参数先校验：越界直接 400，不静默 clamp；省略 limit 仍是"返回全部"。
    let page = match page::PageWindow::parse(&query) {
        Ok(page) => page,
        Err(message) => return err(StatusCode::BAD_REQUEST, "library.invalid", &message),
    };
    // 锁内只取 owned 快照；NFO/stat/过滤/排序/渲染都在锁外的 blocking 预算内。
    let snapshot = {
        let store = state.store.lock();
        let library = match require_visible_library(&store, &id, user_id) {
            Ok(library) => library,
            Err(response) => return response,
        };
        match selection::load_browse_snapshot(&store, &library, user_id) {
            Ok(snapshot) => snapshot,
            Err(response) => return response,
        }
    };
    let files = state.browse_files.clone();
    let work = state
        .http_work
        .run(selection::BROWSE_DEADLINE, move || {
            let library_id = snapshot.library_id.clone();
            let selected = match selection::select_snapshot(snapshot, &query, files.as_ref()) {
                Ok(items) => items,
                Err(response) => return response,
            };
            // 过滤与排序已在 select_snapshot 内完成，分页发生在其后。
            let bounds = page.bounds(selected.len());
            let paged: Vec<_> = selected[bounds]
                .iter()
                .map(|item| wall_item_json(&library_id, item))
                .collect();
            ok_list(paged).into_response()
        })
        .await;
    match work {
        Ok(response) => response,
        Err(error) => selection::browse_work_error(error),
    }
}

fn wall_item_json(library_id: &str, item: &selection::LibrarySelection) -> Value {
    let row = preferred_row(&item.rows);
    json!({
        "media_item_id": item.media.id.to_string(),
        "kind": item.media.kind.as_str(),
        "library_id": library_id,
        "title": item.media.title,
        "year": item.media.year,
        "poster_url": row.filter(|r| poster_path(r).is_some()).map(|r| artwork_url("posters", r.id)),
        "backdrop_url": row.filter(|r| backdrop_path(r).is_some()).map(|r| artwork_url("fanart", r.id)),
        "file_count": item.rows.len(),
        "seasons": item.rows.iter().filter_map(|r| r.season).collect::<std::collections::BTreeSet<_>>(),
        "episode_count": item.rows.iter().filter_map(|r| r.season.zip(r.episode)).collect::<std::collections::HashSet<_>>().len(),
        "max_resolution": selection::best_resolution(&item.rows),
        "missing": item.missing,
    })
}

pub(crate) async fn item_detail(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
) -> Response {
    let media_id = match MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let (media, rows) = {
        let store = state.store.lock();
        match load_owned_rows(&store, &id, media_id, user_id) {
            Ok(loaded) => loaded,
            Err(response) => return response,
        }
    };
    let files = item_files_json(&state, &rows).await;
    let nfo = item_nfo(&media, &rows);
    let cast = item_cast_json(nfo.as_ref());
    let backdrop_url = item_backdrop_url(&rows);
    ok(item_detail_json(
        &media,
        &rows,
        nfo.as_ref(),
        cast,
        backdrop_url,
        files,
    ))
    .into_response()
}

/// Resolve the Media and its Library-owned ledger rows for the item detail view.
fn load_owned_rows(
    store: &crate::Store,
    id: &str,
    media_id: MediaId,
    user_id: Option<domain::UserId>,
) -> Result<(domain::Media, Vec<domain::LedgerRow>), Response> {
    let Some(library) = store.get_library(id).ok().flatten() else {
        return Err(missing_library());
    };
    if !library_visible(store, &library, user_id) {
        return Err(missing_library());
    }
    let Some(media) = store.get_media(media_id).ok().flatten() else {
        return Err(err(
            StatusCode::NOT_FOUND,
            "library.item_missing",
            "条目不存在",
        ));
    };
    // 已知 MediaId 时走定向读取，不再全表扫 ledger 后过滤。
    let rows: Vec<domain::LedgerRow> = store
        .ledger_for_media(media_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|row| {
            library_for_row(store, row, &media).is_some_and(|owner| owner.id == library.id)
        })
        .collect();
    if rows.is_empty() {
        return Err(err(
            StatusCode::NOT_FOUND,
            "library.item_missing",
            "条目不存在",
        ));
    }
    Ok((media, rows))
}

/// file_tracks 已是异步非阻塞：缓存命中即回，未命中入队后台探测并返回空，
/// 详情页绝不会因远程探测等待（并发/超时包装不再需要）。
async fn item_files_json(state: &ApiState, rows: &[domain::LedgerRow]) -> Vec<Value> {
    let mut files = Vec::with_capacity(rows.len());
    for row in rows {
        let tracks = file_tracks(state, row).await;
        files.push(file_json(state, row, &tracks));
    }
    files
}

fn file_json(state: &ApiState, row: &domain::LedgerRow, tracks: &library::Tracks) -> Value {
    json!({
        "file_id": row.id.to_string(),
        "path": row.path,
        "season": row.season,
        "episode": row.episode,
        "resolution": row.resolution,
        "codec": row.codec,
        "hdr": row.hdr,
        "quality_source": row.quality_source.as_str(),
        "probed": tracks.video.is_some()
            || !tracks.audio.is_empty()
            || !tracks.subtitles.is_empty(),
        "probe_queued": state.probe.is_queued(&row.id.to_string()),
        "video_track": tracks.video.as_ref().map(|v| json!({
            "codec": v.codec, "width": v.width, "height": v.height,
            "frame_rate": v.frame_rate, "bit_rate": v.bit_rate,
            "duration_secs": v.duration_secs,
        })),
        "audio_tracks": tracks.audio.iter().map(|t| json!({
            "codec": t.codec, "profile": null, "channels": t.channels,
            "channel_layout": null, "language": t.language,
            "sample_rate": t.sample_rate, "title": null,
            "default": t.is_default,
        })).collect::<Vec<_>>(),
        "subtitle_tracks": tracks.subtitles.iter().map(|t| json!({
            "codec": t.codec, "language": t.language, "title": null,
            "forced": t.forced, "default": t.is_default,
            "external": false, "file_name": null,
        })).collect::<Vec<_>>(),
    })
}

/// 条目级元数据：优先读条目目录的 NFO（Emby/TMM 刮削产物），封面/背景取
/// 本地图片；无 NFO 时字段为空（Block 2 的 TMDB 补充在 metadata/refresh 写入）。
fn item_nfo(media: &domain::Media, rows: &[domain::LedgerRow]) -> Option<library::NfoMeta> {
    let row = preferred_row(rows)?;
    crate::scrape_metadata::nfo_candidates(FsPath::new(&row.path), row, media.kind)
        .into_iter()
        .find(|candidate| candidate.is_file())
        .and_then(|path| library::read_nfo(&path))
}

fn item_backdrop_url(rows: &[domain::LedgerRow]) -> Option<String> {
    let row = preferred_row(rows)?;
    let has_fanart = FsPath::new(&row.path)
        .parent()
        .map(|dir| dir.join("fanart.jpg"))
        .is_some_and(|art| art.is_file());
    has_fanart.then(|| artwork_url("fanart", row.id))
}

fn item_cast_json(nfo: Option<&library::NfoMeta>) -> Vec<Value> {
    nfo.map(|nfo| {
        nfo.cast
            .iter()
            .map(|member| {
                let avatar_url = member.thumb.as_ref().map(|thumb| {
                    if thumb.starts_with("http://") || thumb.starts_with("https://") {
                        thumb.clone()
                    } else {
                        format!("https://image.tmdb.org/t/p/w185{thumb}")
                    }
                });
                json!({
                    "name": member.name,
                    "role": member.role,
                    "tmdb_person_id": member.tmdb_id.as_deref().and_then(|id| id.parse::<i64>().ok()),
                    "avatar_url": avatar_url,
                    "order": member.order,
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

fn item_detail_json(
    media: &domain::Media,
    rows: &[domain::LedgerRow],
    nfo: Option<&library::NfoMeta>,
    cast: Vec<Value>,
    backdrop_url: Option<String>,
    files: Vec<Value>,
) -> Value {
    json!({
        "media_item_id": media.id.to_string(),
        "kind": media.kind.as_str(),
        "title": media.title,
        "year": media.year,
        "original_title": media.original_title,
        "tmdb_id": media.tmdb_id,
        "overview": nfo.and_then(|n| n.plot.clone()),
        "rating": nfo.and_then(|n| n.rating.clone()),
        "vote_count": nfo.and_then(|n| n.vote_count),
        "runtime_minutes": nfo.and_then(|n| n.runtime_minutes.clone()),
        "tagline": nfo.and_then(|n| n.tagline.clone()),
        "premiered": nfo.and_then(|n| n.premiered.clone()),
        "end_date": nfo.and_then(|n| n.end_date.clone()),
        "content_rating": nfo.and_then(|n| n.content_rating.clone()),
        "original_language": nfo.and_then(|n| n.original_language.clone()),
        "status": nfo.and_then(|n| n.status.clone()),
        "number_of_seasons": nfo.and_then(|n| n.number_of_seasons),
        "number_of_episodes": nfo.and_then(|n| n.number_of_episodes),
        "genres": nfo.map(|n| n.genres.clone()).unwrap_or_default(),
        "countries": nfo.map(|n| n.countries.clone()).unwrap_or_default(),
        "studios": nfo.map(|n| n.studios.clone()).unwrap_or_default(),
        "directors": nfo.map(|n| n.directors.clone()).unwrap_or_default(),
        "creators": nfo.map(|n| n.creators.clone()).unwrap_or_default(),
        "cast": cast,
        "poster_url": preferred_row(rows).filter(|row| poster_path(row).is_some()).map(|row| artwork_url("posters", row.id)),
        "backdrop_url": backdrop_url,
        "files": files,
    })
}

pub(crate) async fn item_episodes(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let media_id = match MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    // 锁内只做快 DB 读；TMDB 季详情（同步网络）在锁外 spawn_blocking 跑，
    // 否则冷缓存时 store 锁被长持，me/health 等一切要锁的请求全部超时。
    let season_param = match query.get("season") {
        Some(s) => match s.parse::<u32>() {
            Ok(v) => Some(v),
            Err(error) => {
                tracing::error!(library_id = %id, media_id = %media_id, season = %s, error = %error, "媒体库分集请求季号无效");
                return err(StatusCode::BAD_REQUEST, "library.invalid", "季号参数无效");
            }
        },
        None => None,
    };
    let tmdb_id = {
        let store = state.store.lock();
        let Some(library) = store.get_library(&id).ok().flatten() else {
            return missing_library();
        };
        if !library_visible(&store, &library, user_id) {
            return missing_library();
        }
        let Some(media) = store.get_media(media_id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
        };
        let owned = store
            .list_ledger()
            .unwrap_or_default()
            .into_iter()
            .any(|row| {
                row.media_id == media_id
                    && library_for_row(&store, &row, &media)
                        .is_some_and(|owner| owner.id == library.id)
            });
        if !owned {
            return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
        }
        media.tmdb_id
    };
    // TMDB 整季详情（真实剧名/简介/剧照），fake 测试下为空表 → 退回文件名。
    let mut season_meta: HashMap<u32, media::EpisodeMeta> = HashMap::new();
    if let Some(tmdb_id) = tmdb_id {
        let state_clone = state.clone();
        let tmdb_clone = tmdb_id.clone();
        let seasons: Vec<u32> = season_param.into_iter().collect();
        let fetched = tokio::task::spawn_blocking(move || {
            let catalog: &dyn Catalog = state_clone.catalog.as_ref();
            let mut out: HashMap<u32, Vec<media::EpisodeMeta>> = HashMap::new();
            for season in &seasons {
                let episodes = catalog
                    .season_details(&tmdb_clone, *season)
                    .unwrap_or_default();
                out.insert(*season, episodes);
            }
            out
        })
        .await
        .unwrap_or_default();
        for (_season, metas) in fetched {
            for meta in metas {
                season_meta.insert(meta.episode_number, meta);
            }
        }
    }
    let store = state.store.lock();
    let Some(library) = store.get_library(&id).ok().flatten() else {
        return missing_library();
    };
    let Some(media) = store.get_media(media_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
    };
    let all_rows = store.list_ledger().unwrap_or_default();
    let library_rows: Vec<domain::LedgerRow> = all_rows
        .into_iter()
        .filter(|row| row.media_id == media_id)
        .filter(|row| {
            library_for_row(&store, row, &media).is_some_and(|owner| owner.id == library.id)
        })
        .collect();

    let mut season_metas_grouped: HashMap<u32, HashMap<u32, media::EpisodeMeta>> = HashMap::new();
    if let Some(target_season) = season_param {
        season_metas_grouped.insert(target_season, season_meta);
    }

    let episode_rows = crate::http::playback::episodes::aggregate_visible_episodes(
        &store,
        &media,
        &library_rows,
        season_param,
        user_id,
        &season_metas_grouped,
    );
    let rows = crate::http::playback::episodes::format_episodes_json(&episode_rows, &item_id);
    ok_list(rows).into_response()
}

