//! Library maintenance: in-place scan, metadata/cover refresh, missing
//! episodes, index, and facet dimensions — all scoped to one Library.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::http::{err, ok, ok_list};
use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;

use super::library::{
    missing_library, preferred_row, require_visible_library, rows_in_library, selection,
};

/// POST /libraries/{id}/scan — walk every root of the library and ledger
/// high-confidence video files in place.
pub(crate) async fn scan_library(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    match tokio::task::spawn_blocking(move || scan_library_sync(state, id)).await {
        Ok(response) => response,
        Err(error) => {
            tracing::error!(%error, "媒体库扫描线程异常退出");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "library.scan",
                &format!("扫描任务执行失败: {error}"),
            )
        }
    }
}

#[allow(dead_code)]
pub(crate) fn scan_library_root(state: &ApiState, root: &std::path::Path) -> Result<(), String> {
    scan_library_subdir(state, root, root)
}

pub(crate) fn scan_library_subdir(
    state: &ApiState,
    root: &std::path::Path,
    target_dir: &std::path::Path,
) -> Result<(), String> {
    let (library_id, kind, scrape) = {
        let store = state.store.lock();
        let library = store
            .list_libraries()
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|library| {
                library.realtime_watch
                    && library.root_paths.iter().any(|candidate| candidate == root)
            })
            .ok_or_else(|| format!("no realtime Library configured for root {}", root.display()))?;
        let scrape = store
            .get_scrape_config()
            .map_err(|error| error.to_string())?
            .effective
            .mirror_nfo;
        (library.id, library.kind, scrape)
    };
    let lease = state.acquire_library_work(&library_id)?;
    if !target_dir.is_dir() {
        return Ok(());
    }
    if kind == domain::MediaKind::Video {
        return scan_video_subdir(state, target_dir, root, &library_id, &lease);
    }
    let probe = domain::Media {
        id: domain::MediaId::new(),
        kind,
        title: "watch scan".into(),
        year: None,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let job = library::WatchJob {
        kind: library::WatchKind::InPlace,
        library_root: root.to_path_buf(),
        tv_library_root: None,
        path: target_dir.to_path_buf(),
        scrape,
    };
    let outcome = library::scan_watch(&job, &probe).map_err(|error| error.to_string())?;
    let (scan_media, inserted) = {
        let store = state.store.lock();
        lease.control().check().map_err(|e| e.to_string())?;
        if !store
            .library_accepts_work(&library_id)
            .map_err(|e| e.to_string())?
        {
            return Err("Library 正在删除".into());
        }
        let inserted = crate::watch_ledger::record_paths(&store, outcome.transferred.clone())?;
        let mut scan_media = HashMap::new();
        for file in &outcome.transferred {
            if let Ok(Some(row)) = store.ledger_by_path(&file.path.display().to_string())
                && let Ok(Some(media)) = store.get_media(row.media_id)
                && media.kind != domain::MediaKind::Video
            {
                scan_media
                    .entry(media.id)
                    .or_insert((media, PathBuf::from(row.path)));
            }
        }
        (scan_media, inserted)
    };
    resolve_and_scrape_subdir(state, scan_media, &inserted, scrape, &lease)?;
    tracing::info!(
        root = %root.display(),
        target_dir = %target_dir.display(),
        scanned = outcome.transferred.len(),
        inserted = inserted.len(),
        "实时目录扫描完成，仅新增台账进入探测队列"
    );
    crate::http::library::enqueue_probes_for_paths(state, inserted);
    {
        let store = state.store.lock();
        if let Ok(Some(library)) = store.get_library(&library_id) {
            crate::http::library::clear_library_cover_checked(&store, &library.id);
            let _ = crate::http::library::library_cover_path(&store, &library);
        }
    }
    Ok(())
}

fn scan_video_subdir(
    state: &ApiState,
    target_dir: &std::path::Path,
    root: &std::path::Path,
    library_id: &str,
    lease: &crate::library_lifecycle::LibraryWorkLease,
) -> Result<(), String> {
    let mut files = Vec::new();
    walk_video_files(target_dir, &mut files);
    let inserted = {
        let store = state.store.lock();
        lease.control().check().map_err(|e: library::LibraryError| e.to_string())?;
        if !store
            .library_accepts_work(library_id)
            .map_err(|e| e.to_string())?
        {
            return Err("Library 正在删除".into());
        }
        crate::watch_ledger::record_video_paths(&store, files)?
    };
    tracing::info!(root = %root.display(), target_dir = %target_dir.display(),
        inserted = inserted.len(), "实时 Video Library 扫描完成，不进行 Movie/TV 识别");
    crate::http::library::enqueue_probes_for_paths(state, inserted);
    let store = state.store.lock();
    if let Some(library) = store
        .get_library(library_id)
        .map_err(|error| error.to_string())?
    {
        crate::http::library::clear_library_cover_checked(&store, &library.id);
        let _ = crate::http::library::library_cover_path(&store, &library);
    }
    Ok(())
}

fn resolve_and_scrape_subdir(
    state: &ApiState,
    scan_media: HashMap<domain::MediaId, (domain::Media, PathBuf)>,
    inserted: &[PathBuf],
    scrape: bool,
    lease: &crate::library_lifecycle::LibraryWorkLease,
) -> Result<(), String> {
    for (_, (media, path)) in scan_media {
        lease.control().check().map_err(|e: library::LibraryError| e.to_string())?;
        if crate::auto_resolve::has_local_scan_metadata(state, &media, &path) {
            continue;
        }
        let resolved = if media.tmdb_id.is_some() || media.douban_id.is_some() {
            Some(media.clone())
        } else {
            match crate::auto_resolve::resolve_scanned_media(state, &media, &path) {
                Some(resolved) => {
                    tracing::info!(title = %resolved.title, path = %path.display(), "实时扫描自动匹配媒体元数据成功");
                    Some(resolved)
                }
                None => {
                    tracing::warn!(title = %media.title, path = %path.display(), "实时扫描未能自动匹配媒体元数据");
                    None
                }
            }
        };

        if let Some(resolved_media) = resolved.as_ref()
            && let Err(error) = rebind_scan_rows(state, media.id, resolved_media.id, inserted)
        {
            return Err(error);
        }
        if scrape && let Some(resolved_media) = resolved {
            let _ = crate::poster_fetch::attach_poster(state, &resolved_media, &path);
            let _ = crate::poster_fetch::attach_backdrop(state, &resolved_media, &path);
            let (target_row, all_rows) = {
                let store = state.store.lock();
                let rows = store
                    .ledger_for_media(resolved_media.id)
                    .unwrap_or_default();
                let path_str = path.display().to_string();
                let row = rows.iter().find(|r| r.path == path_str).cloned();
                (row, rows)
            };
            if let Some(row) = target_row {
                scrape_library_metadata(state, &resolved_media, &row, &path, &all_rows);
            }
        }
    }
    Ok(())
}

fn rebind_scan_rows(
    state: &ApiState,
    previous: domain::MediaId,
    canonical: domain::MediaId,
    inserted: &[PathBuf],
) -> Result<(), String> {
    if previous == canonical {
        return Ok(());
    }
    let store = state.store.lock();
    for path in inserted {
        let Some(row) = store
            .ledger_by_path(&path.display().to_string())
            .map_err(|e| e.to_string())?
            .filter(|row| row.media_id == previous)
        else {
            continue;
        };
        let kind = store
            .get_media(canonical)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "canonical Media missing".to_string())?
            .kind;
        let owner = store
            .library_for_path_strict(path, kind)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "scanned path no longer owned".to_string())?;
        if !store
            .library_accepts_work(&owner.id)
            .map_err(|e| e.to_string())?
        {
            return Err("Library 正在删除".into());
        }
        store
            .rebind_scanned_ledger(row.id, canonical)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn scan_library_sync(state: ApiState, id: String) -> Response {
    let lease = match state.acquire_library_work(&id) {
        Ok(lease) => lease,
        Err(error) => return err(StatusCode::CONFLICT, "library.deleting", &error),
    };
    scan_library_leased(&state, &id, &lease)
}

fn scan_library_leased(
    state: &ApiState,
    id: &str,
    lease: &crate::library_lifecycle::LibraryWorkLease,
) -> Response {
    let (kind, roots, scrape) = {
        let store = state.store.lock();
        let Some(library) = store.get_library(&id).ok().flatten() else {
            return missing_library();
        };
        let cfg = store.get_scrape_config().ok();
        let nfo = cfg.as_ref().map(|c| c.effective.mirror_nfo).unwrap_or(true);
        (library.kind, library.root_paths.clone(), nfo)
    };
    let probe = domain::Media {
        id: domain::MediaId::new(),
        kind,
        title: "scan".into(),
        year: None,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    if kind == domain::MediaKind::Video {
        return scan_video_library(&state, &roots, id, lease);
    }
    let mut transferred = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        let job = library::WatchJob {
            kind: library::WatchKind::InPlace,
            library_root: root.clone(),
            tv_library_root: None,
            path: root,
            scrape,
        };
        match library::scan_watch(&job, &probe) {
            Ok(outcome) => transferred.extend(outcome.transferred),
            Err(error) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "library.scan",
                    &error.to_string(),
                );
            }
        }
    }
    let store = state.store.lock();
    if lease.control().is_cancelled() || !store.library_accepts_work(id).unwrap_or(false) {
        return err(StatusCode::CONFLICT, "library.deleting", "Library 正在删除");
    }
    transferred.retain(|file| {
        store
            .library_for_path_strict(&file.path, kind)
            .ok()
            .flatten()
            .is_some_and(|owner| owner.id == id)
    });
    let inserted = match crate::watch_ledger::record_paths(&store, transferred.clone()) {
        Ok(inserted) => inserted,
        Err(error) => return err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error),
    };

    // Resolve scanned media once per title; when NFO mirroring is enabled, write
    // the same full metadata used by explicit refresh into the movie/show NFOs.
    let scan_media = {
        let mut map = std::collections::HashMap::new();
        for file in &transferred {
            if let Ok(Some(row)) = store.ledger_by_path(&file.path.display().to_string()) {
                if let Ok(Some(m)) = store.get_media(row.media_id) {
                    if m.kind != domain::MediaKind::Video {
                        map.entry(m.id).or_insert((m, row));
                    }
                }
            }
        }
        map
    };
    drop(store);

    if let Err(response) = resolve_scan_metadata(state, id, lease, scan_media, &inserted, scrape) {
        return response;
    }
    crate::http::library::enqueue_probes_for_paths(&state, inserted);
    {
        let store = state.store.lock();
        if let Ok(Some(lib)) = store.get_library(&id) {
            crate::http::library::clear_library_cover_checked(&store, &lib.id);
            let _ = crate::http::library::library_cover_path(&store, &lib);
        }
    }
    ok(json!({ "queued": true, "transferred": true })).into_response()
}

fn resolve_scan_metadata(
    state: &ApiState,
    id: &str,
    lease: &crate::library_lifecycle::LibraryWorkLease,
    scan_media: HashMap<domain::MediaId, (domain::Media, domain::LedgerRow)>,
    inserted: &[PathBuf],
    scrape: bool,
) -> Result<(), Response> {
    for (_, (media, row)) in scan_media {
        if lease.control().is_cancelled() {
            return Err(err(
                StatusCode::CONFLICT,
                "library.deleting",
                "Library 正在删除",
            ));
        }
        let path_buf = PathBuf::from(&row.path);
        let path = path_buf.as_path();
        if crate::auto_resolve::has_local_scan_metadata(state, &media, path) {
            continue;
        }
        let resolved = crate::auto_resolve::resolve_scanned_media(state, &media, path)
            .unwrap_or_else(|| media.clone());
        if let Err(error) = rebind_scan_rows(state, media.id, resolved.id, &inserted) {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error,
            ));
        }
        if scrape {
            let _ = crate::poster_fetch::attach_poster(state, &resolved, path);
            let _ = crate::poster_fetch::attach_backdrop(state, &resolved, path);
            // Reread after alias convergence; stale rows still contain the provisional ID.
            let (row, rows) = {
                let store = state.store.lock();
                let row = store
                    .ledger_by_path(&row.path)
                    .ok()
                    .flatten()
                    .unwrap_or(row);
                let rows = store
                    .get_library(id)
                    .ok()
                    .flatten()
                    .map(|library| rows_in_library(&store, &library))
                    .unwrap_or_default();
                (row, rows)
            };
            scrape_library_metadata(state, &resolved, &row, path, &rows);
        }
    }

    Ok(())
}

fn scan_video_library(
    state: &ApiState,
    roots: &[PathBuf],
    library_id: &str,
    lease: &crate::library_lifecycle::LibraryWorkLease,
) -> Response {
    let mut files = Vec::new();
    for root in roots {
        walk_video_files(root, &mut files);
    }
    let inserted = match {
        let store = state.store.lock();
        if lease.control().is_cancelled()
            || !store.library_accepts_work(library_id).unwrap_or(false)
        {
            return err(StatusCode::CONFLICT, "library.deleting", "Library 正在删除");
        }
        crate::watch_ledger::record_video_paths(&store, files.clone())
    } {
        Ok(inserted) => inserted,
        Err(error) => return err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error),
    };
    crate::http::library::enqueue_probes_for_paths(state, inserted);
    for file in files {
        let media = domain::Media {
            id: domain::MediaId::new(),
            kind: domain::MediaKind::Video,
            title: file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string(),
            year: None,
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        let _ = crate::poster_fetch::attach_poster(state, &media, &file);
        let _ = crate::poster_fetch::attach_backdrop(state, &media, &file);
    }
    ok(json!({ "queued": true, "transferred": true })).into_response()
}

const VIDEO_EXTS: &[&str] = &[
    "mkv", "mp4", "ts", "m2ts", "mov", "avi", "m4v", "webm", "wmv", "flv", "strm",
];

fn walk_video_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // 与 Movie/TV 扫描一致：不跟随软链接。根内目录链接指向根外时，
        // 不能把外部视频登记进 Library。
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            tracing::debug!(path = %path.display(), "Video 扫描跳过软链接");
            continue;
        }
        if meta.is_dir() {
            walk_video_files(&path, out);
        } else if meta.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| VIDEO_EXTS.iter().any(|known| e.eq_ignore_ascii_case(known)))
        {
            out.push(path);
        }
    }
}

/// POST /libraries/{id}/metadata/refresh — one cover per Media owned under the
/// library, using the newest owned episode for TV; also writes rich NFO from
/// TMDB details and episode stills for owned TV episodes.
pub(crate) async fn refresh_metadata(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let lease = match state.acquire_library_work(&id) {
        Ok(lease) => lease,
        Err(error) => return err(StatusCode::CONFLICT, "library.deleting", &error),
    };
    let store = state.store.lock();
    let Some(library) = store.get_library(&id).ok().flatten() else {
        return missing_library();
    };
    let owned_rows = rows_in_library(&store, &library);
    let mut grouped: HashMap<domain::MediaId, Vec<domain::LedgerRow>> = HashMap::new();
    for row in &owned_rows {
        grouped.entry(row.media_id).or_default().push(row.clone());
    }
    let mut work = Vec::new();
    for (media_id, media_rows) in grouped {
        let Some(media) = store.get_media(media_id).ok().flatten() else {
            continue;
        };
        if let Some(row) = preferred_row(&media_rows) {
            work.push((media, row.clone()));
        }
    }
    let still_mirror = {
        let config = store.get_scrape_config().ok();
        config
            .as_ref()
            .map(|c| c.effective.mirror_episode_thumbs)
            .unwrap_or(true)
    };
    drop(store);
    let mut refreshed = 0u32;
    for (media, row) in work {
        let path = PathBuf::from(&row.path);
        if crate::poster_fetch::attach_poster(&state, &media, &path).is_some() {
            refreshed += 1;
        }
        // 分集剧照按视频文件命名，同一目录的多集不会共用图片。
        if media.kind == domain::MediaKind::Tv && still_mirror {
            if let Some(tmdb_id) = media.tmdb_id.clone() {
                for row in owned_rows.iter().filter(|r| r.media_id == media.id) {
                    let Some(season) = row.season else { continue };
                    let Some(episode) = row.episode else { continue };
                    let still_path = crate::episode_still::path(std::path::Path::new(&row.path));
                    if still_path.is_file() {
                        continue;
                    }
                    if let Ok(paths) = state.catalog.episode_stills(&tmdb_id, season, episode) {
                        if let Some(file_path) = paths.first() {
                            let size = still_size(&state, &still_mirror);
                            let url = format!("https://image.tmdb.org/t/p/{size}{file_path}");
                            if let Ok(bytes) = state.poster_fetch.get(&url) {
                                if !bytes.is_empty() && !lease.control().is_cancelled() {
                                    let _ = std::fs::write(&still_path, &bytes);
                                }
                            }
                        }
                    }
                }
            }
        }
        let _ = crate::poster_fetch::force_attach_backdrop(&state, &media, &path);
        // TMDB 详情 + 演职员写入 NFO（无 NFO 时也补一份），供条目详情页读取。
        scrape_library_metadata(&state, &media, &row, &path, &owned_rows);
        if lease.control().is_cancelled() {
            return err(StatusCode::CONFLICT, "library.deleting", "Library 正在删除");
        }
        // 章节场景图自动生成（有内嵌章节且无帧时）。
        crate::http::library_chapters::auto_generate_chapters(&state, &path);
    }
    ok(json!({ "queued": true, "refreshed": refreshed })).into_response()
}

fn scrape_library_metadata(
    state: &ApiState,
    media: &domain::Media,
    row: &domain::LedgerRow,
    path: &std::path::Path,
    owned_rows: &[domain::LedgerRow],
) {
    let lease = match state.acquire_path_work(path, media.kind) {
        Ok(lease) => lease,
        Err(_) => return,
    };
    let Some(tmdb_id) = media.tmdb_id.as_deref() else {
        return;
    };
    match crate::scrape_metadata::fetch_tmdb_metadata(state, media.kind, tmdb_id) {
        Ok(Some(metadata)) => {
            if lease
                .as_ref()
                .is_some_and(|lease| lease.control().is_cancelled())
            {
                return;
            }
            let nfo = crate::scrape_metadata::nfo_from_tmdb(media, &metadata);
            if media.kind == domain::MediaKind::Tv {
                let show_root = crate::scrape_metadata::show_root(path, row);
                let episodes = owned_rows
                    .iter()
                    .filter(|episode| episode.media_id == media.id)
                    .cloned()
                    .collect::<Vec<_>>();
                crate::scrape_metadata::write_series_nfos(
                    state,
                    media,
                    tmdb_id,
                    &show_root,
                    &episodes,
                    &nfo,
                    &crate::scrape_metadata::preferred_language(state),
                );
            } else if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
                let target = path.with_file_name(format!("{stem}.nfo"));
                crate::scrape_metadata::write_nfo(&target, media, &nfo);
            }
        }
        Ok(None) => {
            tracing::debug!(%tmdb_id, "catalog returned no metadata during library refresh")
        }
        Err(error) => tracing::warn!(%error, %tmdb_id, "failed to scrape library metadata"),
    }
}

fn still_size(state: &ApiState, _mirror: &bool) -> String {
    state
        .store
        .lock()
        .get_scrape_config()
        .ok()
        .map(|c| c.effective.still_size)
        .filter(|size| !size.is_empty())
        .unwrap_or_else(|| "w300".into())
}

/// GET /libraries/{id}/missing — missing files in this library on disk.
pub(crate) async fn missing(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path(id): Path<String>,
) -> Response {
    let store = state.store.lock();
    let library = match require_visible_library(&store, &id, user_id) {
        Ok(library) => library,
        Err(response) => return response,
    };
    let missing_map = store.missing_at_by_path().unwrap_or_default();
    let mut media_groups: HashMap<domain::MediaId, (domain::Media, Vec<Value>)> = HashMap::new();
    for row in rows_in_library(&store, &library) {
        if missing_map.contains_key(&row.path) {
            if let Some(media) = store.get_media(row.media_id).ok().flatten() {
                let entry = media_groups
                    .entry(row.media_id)
                    .or_insert_with(|| (media, Vec::new()));
                entry.1.push(json!({
                    "id": row.id.to_string(),
                    "file_path": row.path,
                    "season_number": row.season.unwrap_or(0),
                    "episode_number": row.episode.unwrap_or(0),
                    "size_bytes": 0,
                }));
            }
        }
    }
    let items: Vec<Value> = media_groups
        .into_values()
        .map(|(media, files)| {
            json!({
                "media_item_id": media.id.to_string(),
                "kind": media.kind.as_str(),
                "title": media.title,
                "year": media.year,
                "files": files,
            })
        })
        .collect();
    ok_list(items).into_response()
}

/// GET /libraries/{id}/item-index — first-letter index buckets of owned items.
pub(crate) async fn item_index(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let store = state.store.lock();
    let library = match require_visible_library(&store, &id, user_id) {
        Ok(library) => library,
        Err(response) => return response,
    };
    let selected = match selection::select(&store, &library, user_id, &query) {
        Ok(items) => items,
        Err(response) => return response,
    };
    let mut buckets: Vec<Value> = Vec::new();
    let mut current_initial = String::new();
    let mut current_offset = 0usize;
    let mut current_count = 0usize;

    for (i, item) in selected.iter().enumerate() {
        let initial = item
            .media
            .title
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "#".into());
        if initial != current_initial {
            if current_count > 0 {
                buckets.push(json!({
                    "initial": current_initial,
                    "count": current_count,
                    "offset": current_offset,
                }));
            }
            current_initial = initial;
            current_offset = i;
            current_count = 1;
        } else {
            current_count += 1;
        }
    }
    if current_count > 0 {
        buckets.push(json!({
            "initial": current_initial,
            "count": current_count,
            "offset": current_offset,
        }));
    }
    ok_list(buckets).into_response()
}

/// GET /libraries/{id}/facets — filter dimensions.
pub(crate) async fn facets(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path(id): Path<String>,
) -> Response {
    let store = state.store.lock();
    let library = match require_visible_library(&store, &id, user_id) {
        Ok(library) => library,
        Err(response) => return response,
    };
    let mut res_counts: HashMap<String, usize> = HashMap::new();
    let genre_counts: HashMap<String, usize> = HashMap::new();
    let mut total = 0usize;

    for row in rows_in_library(&store, &library) {
        total += 1;
        if let Some(r) = &row.resolution {
            *res_counts.entry(r.clone()).or_default() += 1;
        }
    }
    let mut resolutions: Vec<Value> = res_counts
        .into_iter()
        .map(|(res, count)| {
            json!({
                "value": res.clone(),
                "label": res,
                "count": count,
            })
        })
        .collect();
    resolutions.sort_by(|a, b| {
        let a_str = a["value"].as_str().unwrap_or_default();
        let b_str = b["value"].as_str().unwrap_or_default();
        resolution_rank(b_str).cmp(&resolution_rank(a_str))
    });

    let genres: Vec<Value> = genre_counts
        .into_iter()
        .map(|(g, count)| {
            json!({
                "value": g.clone(),
                "label": g,
                "count": count,
            })
        })
        .collect();

    ok(json!({
        "total": total,
        "genres": genres,
        "resolutions": resolutions,
        "years": [],
    }))
    .into_response()
}

fn resolution_rank(resolution: &str) -> u32 {
    match resolution {
        "2160p" => 4,
        "1080p" => 3,
        "720p" => 2,
        _ => 1,
    }
}
