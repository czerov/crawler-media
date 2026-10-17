use api::{ApiState, Store};
use api::fs_watcher::{StrmGraceTracker, handle_fs_events};
use domain::*;
use notify_debouncer_mini::{DebouncedEvent, DebouncedEventKind};
use std::sync::Arc;

struct NoNetwork;
impl indexer::Fetcher for NoNetwork {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        panic!("regression tests must not access network")
    }
}
impl api::PosterFetch for NoNetwork {
    fn get(&self, _: &str) -> Result<Vec<u8>, String> { panic!("no artwork network") }
}
fn state(tmp: &tempfile::TempDir) -> ApiState {
    ApiState::new(Store::open(tmp.path().join("db")).unwrap(), "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(), Arc::new(NoNetwork),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("staging"))),
        tmp.path().join("library")).unwrap().with_poster_fetch(Arc::new(NoNetwork))
}
fn media() -> Media {
    Media { id: MediaId::new(), kind: MediaKind::Tv, title: "Review".into(), year: Some(2026),
        original_title: None, aliases: Vec::new(), tmdb_id: None, douban_id: None, tvdb_id: None,
        bangumi_id: None, anilist_id: None }
}
fn row(media: &Media, tmp: &tempfile::TempDir, name: &str, episode: u32) -> LedgerRow {
    let path = tmp.path().join(name);
    std::fs::write(&path, b"fixture").unwrap();
    LedgerRow { id: LedgerId::new(), media_id: media.id, path: path.display().to_string(),
        season: Some(1), episode: Some(episode), resolution: None, codec: None, hdr: None,
        quality_source: QualitySource::Release, confidence: Confidence::High, filter_score: None }
}
fn deleted(state: &ApiState, path: &str) {
    handle_fs_events(state, &Arc::new(StrmGraceTracker::new()), vec![DebouncedEvent {
        path: path.into(), kind: DebouncedEventKind::Any }]);
}
fn marker(media: &Media, episode: u32, locked: bool) -> store::StoredMediaMarker {
    store::StoredMediaMarker { media_id: media.id, season: 1, episode,
        intro_start_ms: Some(10_000), intro_end_ms: Some(70_000),
        outro_start_ms: None, outro_end_ms: None, source: "manual".into(), locked, updated_at: 0 }
}

#[test]
fn watcher_preserves_media_still_targeted_by_subscribe() {
    let tmp = tempfile::tempdir().unwrap(); let state = state(&tmp); let media = media();
    let row = row(&media, &tmp, "episode.mkv", 1);
    let sub = Subscribe { id: SubscribeId::new(), user_id: UserId::new(), media_id: media.id,
        coverage: Coverage::Tv { season: 1, episode_from: 1, episode_to: Some(2) },
        fetch_mode: FetchMode::Search, filter_id: FilterId::new(), wash_cut: false,
        wash_cut_filter_id: None, keep_old_versions: false, full_season_pack: false,
        downloader_id: None, library_id: None, tracking_state: "active".into(),
        follow_future: false, search_interval_secs: 1800 };
    { let handle = state.store(); let s = handle.lock();
      s.create_library(MediaKind::Tv, "TV", &[tmp.path().to_str().unwrap()], "everyone", true, &[]).unwrap();
      s.insert_media(&media).unwrap();
      s.insert_ledger(&row).unwrap(); s.insert_subscribe(&sub).unwrap(); }
    std::fs::remove_file(&row.path).unwrap(); deleted(&state, &row.path);
    let handle = state.store(); let s = handle.lock();
    assert!(s.get_subscribe(sub.id).unwrap().is_some());
    assert!(s.get_media(media.id).unwrap().is_some(), "file deletion must not orphan Subscribe");
    assert!(s.missing_at_by_path().unwrap().contains_key(&row.path), "file deletion marks missing rather than deleting ledger row");
    assert!(s.ledger_by_path(&row.path).unwrap().is_some(), "ledger row preserved until admin prune");
}

#[test]
fn deleting_one_version_preserves_locked_marker_for_other_version() {
    let tmp = tempfile::tempdir().unwrap(); let state = state(&tmp); let media = media();
    let a = row(&media, &tmp, "version-a.mkv", 1); let b = row(&media, &tmp, "version-b.mkv", 1);
    { let handle = state.store(); let s = handle.lock(); s.insert_media(&media).unwrap();
      s.insert_ledger(&a).unwrap(); s.insert_ledger(&b).unwrap();
      s.put_media_marker(&marker(&media, 1, true)).unwrap(); }
    std::fs::remove_file(&a.path).unwrap(); deleted(&state, &a.path);
    let handle = state.store(); let s = handle.lock();
    assert!(s.get_ledger(&b.id.to_string()).unwrap().is_some());
    assert!(s.get_media_marker(media.id, Some(1), Some(1)).unwrap().is_some(),
        "another version still owns the locked episode marker");
}

#[test]
fn repeated_empty_chapter_read_does_not_borrow_neighbor_intro() {
    let tmp = tempfile::tempdir().unwrap(); let state = state(&tmp); let media = media();
    let neighbor = row(&media, &tmp, "e1.strm", 1); let special = row(&media, &tmp, "e2.strm", 2);
    { let handle = state.store(); let s = handle.lock(); s.insert_media(&media).unwrap();
      s.insert_ledger(&neighbor).unwrap(); s.insert_ledger(&special).unwrap();
      s.put_media_marker(&marker(&media, 1, false)).unwrap(); }
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let first = runtime.block_on(api::marker_resolver::resolve_item_chapters(&state, &special, &media, None));
    let second = runtime.block_on(api::marker_resolver::resolve_item_chapters(&state, &special, &media, None));
    assert!(first.is_empty());
    assert!(second.is_empty(), "no new evidence supports a different episode intro");
}

#[test]
fn realtime_video_library_uses_video_ingestion() {
    let tmp = tempfile::tempdir().unwrap(); let state = state(&tmp);
    let root = tmp.path().join("home"); std::fs::create_dir(&root).unwrap();
    let file = root.join("Vacation.2026.1080p.mp4"); std::fs::write(&file, b"fixture").unwrap();
    { let handle = state.store(); let s = handle.lock();
      let lib = s.create_library(MediaKind::Video, "Home", &[root.to_str().unwrap()], "everyone", true, &[]).unwrap();
      s.set_library_switch_settings(&lib.id, Some(true), None, None, None, None).unwrap(); }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        handle_fs_events(&state, &Arc::new(StrmGraceTracker::new()), vec![DebouncedEvent {
            path: file.clone(), kind: DebouncedEventKind::Any }]);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let kind = { let handle = state.store(); let s = handle.lock();
                    s.ledger_by_path(file.to_str().unwrap()).unwrap()
                    .and_then(|r| s.get_media(r.media_id).unwrap()).map(|m| m.kind) };
                if let Some(kind) = kind { assert_eq!(kind, MediaKind::Video); break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.expect("event must ingest file");
    });
}
