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
        store.list_libraries().map_err(|e| e.to_string())?.into_iter()
            .find(|library| library.root_paths.iter().any(|root| root == &path))
    };
    let lease = owner.map(|owner| state.acquire_library_work(&owner.id)).transpose()?;
    tracing::debug!(dir = %path.display(), "扫描目录，刮削并生成 NFO");
    let probe = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "inplace".into(),
        year: None,
        original_title: None,
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
    let outcome = scan_watch(&job, &probe).map_err(|e| {
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
        if let Some(lease) = &lease { lease.control().check().map_err(|e| e.to_string())?; }
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
        let _ = crate::auto_resolve::auto_resolve_media(state, &media, &dest);
    }
    crate::http::library::enqueue_probes_for_paths(state, inserted);

    // 自动补封面：目录内缺 poster.jpg 的文件从 TMDB 拉海报 + 背板
    // （Emby/Jellyfin 式「没封面自动设置」；已有封面跳过，手动选过的不会被覆盖）。
    // 注意：先收集目标再逐个补图——attach_poster 内部会再锁 store，
    // 持锁调用是死锁。
    let targets: Vec<(domain::MediaId, PathBuf)> = {
        let dir_to_scan = job.path.clone();
        let store = state.store.lock();
        store
            .list_ledger()
            .unwrap_or_default()
            .into_iter()
            .filter(|row| std::path::Path::new(&row.path).starts_with(&dir_to_scan))
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
        let _ = crate::poster_fetch::attach_poster(&state, &real, &row_path);
        let _ = crate::poster_fetch::attach_backdrop(&state, &real, &row_path);
    }
    Ok(())
}
