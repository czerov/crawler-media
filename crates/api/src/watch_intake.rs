use std::path::PathBuf;

use domain::{Media, MediaId, MediaKind};
use library::{WatchJob, WatchKind, scan_watch_intake_controlled};

use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;
pub fn run(state: &ApiState) -> Result<(), String> {
    let (intake, movie_library, tv_library, nfo) = {
        let store = state.store.lock();
        let Some(path) = store.watch_intake().map_err(|e| e.to_string())? else {
            return Ok(());
        };
        if path.is_empty() {
            return Ok(());
        }
        let movie = store
            .default_library(MediaKind::Movie)
            .map_err(|e| e.to_string())?
            .ok_or("Movie Library 不存在")?;
        let tv = store
            .default_library(MediaKind::Tv)
            .map_err(|e| e.to_string())?;
        let nfo = store
            .get_scrape_config()
            .map_err(|e| e.to_string())?
            .effective
            .mirror_nfo;
        (PathBuf::from(path), movie, tv, nfo)
    };
    if !intake.is_dir() {
        return Ok(());
    }
    let movie_lease = state.acquire_library_work(&movie_library.id)?;
    let tv_lease = tv_library
        .as_ref()
        .map(|library| state.acquire_library_work(&library.id))
        .transpose()?;
    let library_root = movie_library
        .root_paths
        .first()
        .cloned()
        .ok_or("Movie Library 没有根目录")?;
    let tv_library_root = tv_library
        .as_ref()
        .and_then(|library| library.root_paths.first().cloned());
    let probe = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "intake".into(),
        year: None,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let job = WatchJob {
        kind: WatchKind::Intake,
        path: intake,
        library_root,
        tv_library_root,
        scrape: nfo,
    };
    let outcome = scan_watch_intake_controlled(
        &job,
        &probe,
        movie_lease.control(),
        tv_lease.as_ref().map(|lease| lease.control()),
    )
    .map_err(|e| e.to_string())?;
    let transferred = outcome.transferred;
    {
        let store = state.store.lock();
        check_intake_libraries(&store, &movie_lease, tv_lease.as_ref())?;
        for parked in outcome.unidentified {
            store
                .insert_unidentified(parked.path.display().to_string(), parked.confidence)
                .map_err(|e| e.to_string())?;
        }
    }
    let inserted = {
        let store = state.store.lock();
        check_intake_libraries(&store, &movie_lease, tv_lease.as_ref())?;
        crate::watch_ledger::record_paths(&store, transferred.clone())?
    };

    finish_intake_metadata(state, &transferred, inserted, &movie_lease, tv_lease.as_ref(), nfo)?;
    // Intake 写入 ledger / Media / NFO：目标 Library 的投影必须重建。
    state.projection.mark_dirty(movie_lease.library_id());
    if let Some(lease) = tv_lease.as_ref() {
        state.projection.mark_dirty(lease.library_id());
    }
    if !outcome.errors.is_empty() {
        let err_msgs: Vec<String> = outcome
            .errors
            .iter()
            .map(|e| format!("{}: {}", e.path.display(), e.error))
            .collect();
        return Err(format!(
            "watch intake completed with {} error(s): {}",
            outcome.errors.len(),
            err_msgs.join("; ")
        ));
    }
    Ok(())
}

fn finish_intake_metadata(
    state: &ApiState,
    transferred: &[library::TransferredFile],
    inserted: Vec<PathBuf>,
    movie_lease: &crate::library_lifecycle::LibraryWorkLease,
    tv_lease: Option<&crate::library_lifecycle::LibraryWorkLease>,
    nfo: bool,
) -> Result<(), String> {
    // 为没有 TMDB ID 的影视条目自动匹配 TMDB 元数据与海报
    let unresolved_media = {
        let store = state.store.lock();
        let mut map = std::collections::HashMap::new();
        for dest in transferred {
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
        check_intake_libraries(&state.store.lock(), movie_lease, tv_lease)?;
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

    // 自动补封面：目标媒体库目录下缺 poster.jpg 的文件从 TMDB 拉海报 + 背板
    let targets: Vec<(domain::MediaId, PathBuf)> = {
        let store = state.store.lock();
        transferred
            .iter()
            .filter_map(|t| {
                let row = store
                    .ledger_by_path(&t.path.display().to_string())
                    .ok()
                    .flatten()?;
                let missing_poster = t
                    .path
                    .parent()
                    .map(|dir| !dir.join("poster.jpg").is_file())
                    .unwrap_or(false);
                if missing_poster {
                    Some((row.media_id, t.path.clone()))
                } else {
                    None
                }
            })
            .collect()
    };
    for (media_id, row_path) in targets {
        check_intake_libraries(&state.store.lock(), movie_lease, tv_lease)?;
        let Some(real) = state.store.lock().get_media(media_id).ok().flatten() else {
            continue;
        };
        let _ = crate::poster_fetch::attach_poster(state, &real, &row_path);
        let _ = crate::poster_fetch::attach_backdrop(state, &real, &row_path);
    }
    Ok(())
}

fn check_intake_libraries(
    store: &crate::Store,
    movie: &crate::library_lifecycle::LibraryWorkLease,
    tv: Option<&crate::library_lifecycle::LibraryWorkLease>,
) -> Result<(), String> {
    for lease in std::iter::once(movie).chain(tv) {
        lease.control().check().map_err(|e| e.to_string())?;
        if !store
            .library_accepts_work(lease.library_id())
            .map_err(|e| e.to_string())?
        {
            return Err("Watch intake 的目标 Library 正在删除或已不存在".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use downloader::MemoryDownloader;
    use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};

    use super::*;

    struct EmptyFetcher;

    impl Fetcher for EmptyFetcher {
        fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
            Err(IndexerError::Fetch("unexpected request".into()))
        }
    }

    #[test]
    fn transferred_media_is_queued_for_probe_without_waiting_for_detail_view() {
        let tmp = tempfile::tempdir().unwrap();
        let intake = tmp.path().join("intake");
        std::fs::create_dir_all(&intake).unwrap();
        std::fs::write(intake.join("The.Matrix.1999.2160p.BluRay.mkv"), b"media").unwrap();
        let store = crate::Store::open(tmp.path().join("data")).unwrap();
        store.set_watch_intake(intake.to_str().unwrap()).unwrap();
        let state = ApiState::new(
            store,
            "test-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(EmptyFetcher),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
            tmp.path().join("library"),
        )
        .unwrap();

        run(&state).unwrap();

        let rows = state.store.lock().list_ledger().unwrap();
        assert_eq!(rows.len(), 1);
        assert!(
            state.probe.is_queued(&rows[0].id.to_string()),
            "new ledger rows should start media probing during intake"
        );
    }

    #[test]
    fn newly_added_tv_episode_queues_metadata_without_voiceprint_when_voiceprint_is_off() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::Store::open(tmp.path().join("data")).unwrap();
        let state = ApiState::new(
            store,
            "test-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(EmptyFetcher),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
            tmp.path().join("library"),
        )
        .unwrap();
        let tv_library = state
            .store
            .lock()
            .default_library(MediaKind::Tv)
            .unwrap()
            .unwrap();
        assert!(!tv_library.detect_intros);
        assert!(!tv_library.enable_fingerprint);
        let root = tv_library.root_paths.first().unwrap();
        let episode_path = root.join("Pantheon").join("Pantheon.S01E01.mkv");
        std::fs::create_dir_all(episode_path.parent().unwrap()).unwrap();
        std::fs::write(&episode_path, b"media fixture").unwrap();

        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "万神殿".into(),
            year: Some(2022),
            original_title: Some("Pantheon".into()),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        let media = state.store.lock().ensure_media(media).unwrap();
        let row = domain::LedgerRow {
            id: domain::LedgerId::new(),
            media_id: media.id,
            path: episode_path.display().to_string(),
            season: Some(1),
            episode: Some(1),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Release,
            confidence: domain::Confidence::High,
            filter_score: None,
        };
        state.store.lock().insert_ledger(&row).unwrap();

        assert_eq!(
            crate::http::library::enqueue_probes_for_rows(&state, &[row]),
            1
        );
        let queued = state.probe.take_queued_for_test().unwrap();
        assert_eq!(queued.kind, MediaKind::Tv);
        assert!(
            !queued.force_fingerprint,
            "metadata ingestion should not override the library voiceprint toggle"
        );
    }

    #[test]
    fn newly_added_tv_episode_runs_voiceprint_when_chapter_detection_is_off() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::Store::open(tmp.path().join("data")).unwrap();
        let state = ApiState::new(
            store,
            "test-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(EmptyFetcher),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
            tmp.path().join("library"),
        )
        .unwrap();
        let tv_library = state
            .store
            .lock()
            .default_library(MediaKind::Tv)
            .unwrap()
            .unwrap();
        state
            .store
            .lock()
            .set_library_intro_settings(&tv_library.id, true, true)
            .unwrap();
        let episode_path = tv_library.root_paths[0]
            .join("Pantheon")
            .join("Pantheon.S01E01.mkv");
        std::fs::create_dir_all(episode_path.parent().unwrap()).unwrap();
        std::fs::write(&episode_path, b"media fixture").unwrap();

        let media = state
            .store
            .lock()
            .ensure_media(Media {
                id: MediaId::new(),
                kind: MediaKind::Tv,
                title: "万神殿".into(),
                year: Some(2022),
                original_title: Some("Pantheon".into()),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let row = domain::LedgerRow {
            id: domain::LedgerId::new(),
            media_id: media.id,
            path: episode_path.display().to_string(),
            season: Some(1),
            episode: Some(1),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Release,
            confidence: domain::Confidence::High,
            filter_score: None,
        };
        state.store.lock().insert_ledger(&row).unwrap();

        assert_eq!(
            crate::http::library::enqueue_probes_for_rows(&state, &[row]),
            1
        );
        let queued = state.probe.take_queued_for_test().unwrap();
        assert!(queued.force_fingerprint);
    }
}
