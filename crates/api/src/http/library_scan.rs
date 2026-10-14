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

pub(crate) mod scope;
pub(crate) mod operations;
pub(crate) mod report;
pub(crate) mod video;
use report::format_scan_report;
use scope::{ScanScope, walk_video_files};
use video::scan_video_library;
use super::library::{
    facets, missing_library, preferred_row, require_visible_library, rows_in_library, selection,
};

/// POST /libraries/{id}/scan — walk every root of the library and ledger
/// high-confidence video files in place.
pub(crate) async fn scan_library(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    match state.scan_coordinator.enqueue(&state, &id, vec![]) {
        Ok(op) => (
            StatusCode::ACCEPTED,
            axum::Json(serde_json::json!({
                "ok": true,
                "data": {
                    "operation_id": op.id,
                    "status": op.status.as_str(),
                    "queued": true,
                    "transferred": false,
                }
            })),
        )
            .into_response(),
        Err(crate::scan_operations::ScanEnqueueError::NotFound(e)) => {
            err(StatusCode::NOT_FOUND, "library.missing", &e)
        }
        Err(crate::scan_operations::ScanEnqueueError::Conflict(e)) => {
            err(StatusCode::CONFLICT, "library.deleting", &e)
        }
        Err(crate::scan_operations::ScanEnqueueError::Internal(e)) => {
            err(StatusCode::INTERNAL_SERVER_ERROR, "library.scan", &e)
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
    let scan_scope = ScanScope::capture(state, &library_id).map_err(|e| format!("{:?}", e))?;
    let filtered_transferred = scan_scope
        .filter_transferred(outcome.transferred)
        .map_err(|e| format!("{:?}", e))?;

    let inserted = lease.publish(|| {
        let store = state.store.lock();
        scan_scope.validate(&store).map_err(|e| format!("{:?}", e))?;
        crate::watch_ledger::record_paths(&store, filtered_transferred.clone())
    })?;

    let scan_media = {
        let store = state.store.lock();
        let mut map = HashMap::new();
        for file in &filtered_transferred {
            if let Ok(Some(row)) = store.ledger_by_path(&file.path.display().to_string())
                && let Ok(Some(media)) = store.get_media(row.media_id)
                && media.kind != domain::MediaKind::Video
            {
                map.entry(media.id)
                    .or_insert((media, PathBuf::from(row.path)));
            }
        }
        map
    };
    resolve_and_scrape_subdir(state, scan_media, &inserted, scrape, &lease)?;
    tracing::info!(
        root = %root.display(),
        target_dir = %target_dir.display(),
        scanned = filtered_transferred.len(),
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
    let scan_scope = ScanScope::capture(state, library_id).map_err(|e| format!("{:?}", e))?;
    let filtered_files = scan_scope.filter_paths(files).map_err(|e| format!("{:?}", e))?;
    let inserted = lease.publish(|| {
        let store = state.store.lock();
        scan_scope.validate(&store).map_err(|e| format!("{:?}", e))?;
        crate::watch_ledger::record_video_paths(&store, filtered_files)
    })?;
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
        lease
            .control()
            .check()
            .map_err(|e: library::LibraryError| e.to_string())?;
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
            && let Err(error) =
                crate::auto_resolve::rebind_scan_rows(state, media.id, resolved_media.id, inserted)
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

fn scan_library_sync(state: ApiState, id: String) -> Response {
    let lease = match state.acquire_library_work(&id) {
        Ok(lease) => lease,
        Err(error) => return err(StatusCode::CONFLICT, "library.deleting", &error),
    };
    scan_library_leased(&state, &id, &lease)
}

pub(crate) fn scan_library_leased(
    state: &ApiState,
    id: &str,
    lease: &crate::library_lifecycle::LibraryWorkLease,
) -> Response {
    match scan_library_leased_result(state, id, lease) {
        Ok((_, report_data)) => ok(report_data).into_response(),
        Err(resp) => resp,
    }
}

fn walk_library_roots(
    state: &ApiState,
    roots: &[PathBuf],
    scrape: bool,
    probe: &domain::Media,
) -> Result<(Vec<library::TransferredFile>, Vec<library::ScanRootReport>), Response> {
    let mut transferred = Vec::new();
    let mut root_reports = Vec::new();
    let mut any_readable = false;
    for root in roots {
        let is_readable = state.library_files.readable_root(root).is_ok();
        if is_readable {
            any_readable = true;
        }
        let job = library::WatchJob {
            kind: library::WatchKind::InPlace,
            library_root: root.clone(),
            tv_library_root: None,
            path: root.clone(),
            scrape,
        };
        match library::scan_watch_reported(&job, probe) {
            Ok((outcome, r_report)) => {
                transferred.extend(outcome.transferred);
                root_reports.push(r_report);
            }
            Err(error) => {
                return Err(err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "library.scan",
                    &error.to_string(),
                ));
            }
        }
    }
    if !any_readable {
        return Err(err(StatusCode::CONFLICT, "library.root_unavailable", "所有库根目录均不可达"));
    }
    Ok((transferred, root_reports))
}

pub(crate) fn scan_library_leased_result(
    state: &ApiState,
    id: &str,
    lease: &crate::library_lifecycle::LibraryWorkLease,
) -> Result<(library::ScanReport, Value), Response> {
    let (kind, roots, scrape) = {
        let store = state.store.lock();
        let Some(library) = store.get_library(&id).ok().flatten() else {
            return Err(missing_library());
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
        return scan_video_library(state, &roots, id, lease);
    }
    let (transferred, root_reports) = walk_library_roots(state, &roots, scrape, &probe)?;
    let scan_scope = match ScanScope::capture(state, id) {
        Ok(s) => s,
        Err(crate::library_presence::PresenceError::Deleting) => {
            return Err(err(StatusCode::CONFLICT, "library.deleting", "Library 正在删除"));
        }
        Err(crate::library_presence::PresenceError::ScopeChanged) => {
            return Err(err(StatusCode::CONFLICT, "library.scope_changed", "Library 配置变化"));
        }
        Err(crate::library_presence::PresenceError::Ambiguous) => {
            return Err(err(StatusCode::CONFLICT, "library.ownership_ambiguous", "媒体库归属歧义"));
        }
        Err(e) => return Err(err(StatusCode::CONFLICT, "library.scan", &format!("{:?}", e))),
    };
    let filtered_transferred = match scan_scope.filter_transferred(transferred) {
        Ok(t) => t,
        Err(e) => return Err(err(StatusCode::CONFLICT, "library.ownership_ambiguous", &format!("{:?}", e))),
    };
    let inserted = match lease.publish(|| {
        let store = state.store.lock();
        scan_scope.validate(&store).map_err(|e| format!("{:?}", e))?;
        crate::watch_ledger::record_paths(&store, filtered_transferred.clone())
    }) {
        Ok(inserted) => inserted,
        Err(error) => {
            if error.contains("Deleting") {
                return Err(err(StatusCode::CONFLICT, "library.deleting", "Library 正在删除"));
            }
            if error.contains("ScopeChanged") {
                return Err(err(StatusCode::CONFLICT, "library.scope_changed", "Library 配置变化"));
            }
            return Err(err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error));
        }
    };

    // Resolve scanned media once per title using filtered_transferred
    let scan_media = {
        let store = state.store.lock();
        let mut map = std::collections::HashMap::new();
        for file in &filtered_transferred {
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

    if let Err(response) = resolve_scan_metadata(state, id, lease, scan_media, &inserted, scrape) {
        return Err(response);
    }
    crate::http::library::enqueue_probes_for_paths(&state, inserted.clone());
    {
        let store = state.store.lock();
        if let Ok(Some(lib)) = store.get_library(&id) {
            crate::http::library::clear_library_cover_checked(&store, &lib.id);
            let _ = crate::http::library::library_cover_path(&store, &lib);
        }
    }
    let total_errors: usize = root_reports.iter().map(|r| r.error_count).sum();
    let scan_report = library::ScanReport {
        roots: root_reports,
        scanned: filtered_transferred.len(),
        inserted: inserted.len(),
        restored: 0,
        probe_pending: inserted.len(),
        error_count: total_errors,
        errors_truncated: false,
    };
    let report_data = format_scan_report(&scan_report);
    Ok((scan_report, json!(report_data)))
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
        if let Err(error) =
            crate::auto_resolve::rebind_scan_rows(state, media.id, resolved.id, &inserted)
        {
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
        let live_rows: Vec<_> = media_rows
            .iter()
            .filter(|row| {
                crate::scrape_metadata::targets::video_present(std::path::Path::new(&row.path))
            })
            .collect();
        if let Some(row) = preferred_row(&live_rows) {
            work.push((media, (*row).clone()));
        }
    }
    drop(store);
    let mut refreshed = 0u32;
    for (media, row) in work {
        let path = PathBuf::from(&row.path);
        if crate::poster_fetch::attach_poster(&state, &media, &path).is_some() {
            refreshed += 1;
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
    if media.kind == domain::MediaKind::Tv {
        let show_root = crate::scrape_metadata::show_root(path, row);
        let episodes = owned_rows
            .iter()
            .filter(|episode| episode.media_id == media.id)
            .cloned()
            .collect::<Vec<_>>();
        crate::scrape_metadata::scrape_tv_sidecars(state, media, &show_root, &episodes);
        return;
    }
    match crate::scrape_metadata::fetch_tmdb_metadata(state, media.kind, tmdb_id) {
        Ok(Some(metadata)) => {
            if lease
                .as_ref()
                .is_some_and(|lease| lease.control().is_cancelled())
            {
                return;
            }
            let nfo = crate::scrape_metadata::nfo_from_tmdb(media, &metadata);
            if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
                let target = path.with_file_name(format!("{stem}.nfo"));
                let write = || {
                    crate::scrape_metadata::write_nfo(&target, media, &nfo);
                    Ok(())
                };
                let _ = match &lease {
                    Some(lease) => lease.publish(write),
                    None => write(),
                };
                let movie_root = path.parent().unwrap_or(path);
                crate::scrape_metadata::scrape_movie_fanart(state, media, movie_root);
            }
        }
        Ok(None) => {
            tracing::debug!(%tmdb_id, "catalog returned no metadata during library refresh")
        }
        Err(error) => tracing::warn!(%error, %tmdb_id, "failed to scrape library metadata"),
    }
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
    // 锁内只取 owned 快照；NFO/stat/过滤/排序都在锁外的 blocking 预算内。
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
            let selected = match selection::select_snapshot(snapshot, &query, files.as_ref()) {
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
        })
        .await;
    match work {
        Ok(response) => response,
        Err(error) => selection::browse_work_error(error),
    }
}

/// GET /libraries/{id}/facets — 十维筛选候选与「排除自身维」计数。
pub(crate) async fn facets(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    // tier：缺失/空串按 all（向后兼容旧调用方仍能拿到 resolutions）；其余非法值 400。
    let tier = match query.get("tier").map(String::as_str) {
        None | Some("") | Some("all") => "all",
        Some("primary") => "primary",
        Some(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "library.invalid",
                "tier 必须是 primary 或 all",
            );
        }
    };
    // 锁内只取 owned 快照；NFO/stat/匹配/计数全部在锁外的 blocking 预算内。
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
            let items = selection::select_all_for_facets(snapshot, files.as_ref());
            let facets = match facets::build_facets(&items, &query, tier) {
                Ok(facets) => facets,
                Err(message) => {
                    tracing::error!(%message, "Library facets query rejected");
                    return err(StatusCode::BAD_REQUEST, "library.invalid", &message);
                }
            };
            match serde_json::to_value(&facets) {
                Ok(value) => ok(value).into_response(),
                Err(error) => {
                    tracing::error!(%error, "failed to serialize library facets");
                    err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "store.error",
                        &error.to_string(),
                    )
                }
            }
        })
        .await;
    match work {
        Ok(response) => response,
        Err(error) => selection::browse_work_error(error),
    }
}
