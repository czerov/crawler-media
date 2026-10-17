use std::path::PathBuf;

use domain::{Media, MediaId, MediaKind};
use library::{WatchJob, WatchKind, scan_watch};

use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;

pub fn run(state: &ApiState) -> Result<(), String> {
    let (dirs, nfo) = {
        let store = state.store.lock();
        let mut dirs = Vec::new();
        if let Some(path) = store.watch_inplace().map_err(|e| e.to_string())? {
            dirs.push(PathBuf::from(path));
        }
        // Extra roots attached to default Libraries are still distinct watched
        // roots; only the primary/default root is excluded from this pass.
        for root in store.list_library_roots().map_err(|e| e.to_string())? {
            if !root.is_default {
                dirs.push(root.path);
            }
        }
        let nfo = store
            .get_scrape_config()
            .map_err(|e| e.to_string())?
            .effective
            .mirror_nfo;
        (dirs, nfo)
    };
    for path in dirs {
        scrape_dir(state, path, nfo)?;
    }
    Ok(())
}

fn scrape_dir(state: &ApiState, path: PathBuf, nfo: bool) -> Result<(), String> {
    if path.as_os_str().is_empty() || !path.is_dir() {
        return Ok(());
    }
    let owner = {
        let store = state.store.lock();
        let mut owners = Vec::new();
        for kind in [MediaKind::Movie, MediaKind::Tv, MediaKind::Video] {
            if let Some(owner) = store.library_for_path_strict(&path, kind).map_err(|e| e.to_string())? { owners.push(owner); }
        }
        owners.into_iter().max_by_key(|library| library.root_paths.iter().filter(|root| path.starts_with(root))
            .map(|root| root.components().count()).max().unwrap_or(0))
    };
    if owner.is_none() && state.store.lock().path_in_removal_scope(&path).map_err(|e| e.to_string())? {
        return Ok(());
    }
    let kind = owner.as_ref().map(|owner| owner.kind).unwrap_or(MediaKind::Movie);
    let lease = owner.map(|owner| state.acquire_library_work(&owner.id)).transpose()?;
    tracing::debug!(dir = %path.display(), "扫描目录，刮削并生成 NFO");
    let probe = Media {
        id: MediaId::new(),
        kind,
        title: "inplace".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let job = WatchJob {
        kind: WatchKind::InPlace,
        library_root: path.clone(),
        tv_library_root: None,
        path: path.clone(),
        scrape: nfo,
    };
    let mut outcome = scan_watch(&job, &probe).map_err(|e| {
        tracing::error!(dir = %path.display(), error = %e, "目录扫描失败");
        e.to_string()
    })?;
    if !outcome.transferred.is_empty() {
        tracing::info!(
            dir = %path.display(),
            items = outcome.transferred.len(),
            "已记录目录中新增刮削的媒体路径"
        );
    }
    let inserted = {
        let store = state.store.lock();
        if let Some(lease) = &lease {
            lease.control().check().map_err(|e| e.to_string())?;
            if !store.library_accepts_work(lease.library_id()).map_err(|e| e.to_string())? { return Ok(()); }
            outcome.transferred.retain(|file| store.library_for_path_strict(&file.path, kind).ok().flatten()
                .is_some_and(|owner| owner.id == lease.library_id()));
        } else {
            outcome.transferred.retain(|file| !store.path_in_removal_scope(&file.path).unwrap_or(true));
        }
        crate::watch_ledger::record_paths(&store, outcome.transferred.clone())?
    };

    // MovieClaw / MoviePilot 模式：扫描入库后，为没有 TMDB ID 的影视条目自动匹配 TMDB 元数据与海报
    let unresolved_media = {
        let store = state.store.lock();
        let mut map = std::collections::HashMap::new();
        for dest in &outcome.transferred {
            if let Ok(Some(row)) = store.ledger_by_path(&dest.path.display().to_string()) {
                if let Ok(Some(m)) = store.get_media(row.media_id) {
                    if m.tmdb_id.is_none() && m.kind != domain::MediaKind::Video {
                        map.entry(m.id).or_insert((m, dest.path.clone()));
                    }
                }
            }
        }
        map
    };
    for (_, (media, dest)) in unresolved_media {
        let resolved = crate::auto_resolve::resolve_scanned_media(state, &media, &dest);
        let active_media = if let Some(canonical) = resolved {
            crate::auto_resolve::rebind_scan_rows(state, media.id, canonical.id, &inserted)?;
            canonical
        } else {
            media
        };
        if nfo && active_media.tmdb_id.is_some() {
            let _ = crate::poster_fetch::attach_poster(state, &active_media, &dest);
            let _ = crate::poster_fetch::attach_backdrop(state, &active_media, &dest);
            crate::auto_resolve::write_resolved_nfo(state, &active_media, &dest);
        }
    }
    crate::http::library::enqueue_probes_for_paths(state, inserted);
    attach_missing_posters(state, &job.path);
    Ok(())
}

fn attach_missing_posters(state: &ApiState, dir_to_scan: &std::path::Path) {
    let targets: Vec<(domain::MediaId, PathBuf)> = {
        let store = state.store.lock();
        store
            .list_ledger()
            .unwrap_or_default()
            .into_iter()
            .filter(|row| std::path::Path::new(&row.path).starts_with(dir_to_scan))
            .filter(|row| {
                std::path::Path::new(&row.path)
                    .parent()
                    .map(|dir| !dir.join("poster.jpg").is_file())
                    .unwrap_or(false)
            })
            .map(|row| (row.media_id, PathBuf::from(&row.path)))
            .collect()
    };
    for (media_id, row_path) in targets {
        let Some(real) = state.store.lock().get_media(media_id).ok().flatten() else {
            continue;
        };
        let _ = crate::poster_fetch::attach_poster(state, &real, &row_path);
        let _ = crate::poster_fetch::attach_backdrop(state, &real, &row_path);
    }
}
