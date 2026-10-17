use api::fs_watcher::StrmGraceTracker;
use notify_debouncer_mini::DebouncedEvent;
use std::sync::Arc;

struct EmptyFetcher;

impl indexer::Fetcher for EmptyFetcher {
    fn fetch(&self, _request: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch("unexpected request".into()))
    }
}

#[tokio::test]
async fn test_directory_deletion_removes_nested_strm_and_cleans_up_media() {
    let temp = tempfile::tempdir().unwrap();
    let library_root = temp.path().join("media/movie/china");
    let movie_dir = library_root.join("“骗骗”喜欢你 (2024)");
    std::fs::create_dir_all(&movie_dir).unwrap();

    let strm_file = movie_dir.join("“骗骗”喜欢你 (2024) - 2160p.strm");
    std::fs::write(&strm_file, "https://example.com/video.mkv").unwrap();

    let store = api::Store::open(temp.path().join("data")).unwrap();
    store
        .set_library_root(domain::MediaKind::Movie, library_root.to_str().unwrap())
        .unwrap();

    let state = api::management::ApiState::new(
        store,
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();

    // 1. 入账并关联媒体
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "“骗骗”喜欢你".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("12345".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    state.store().lock().insert_media(&media).unwrap();

    let row = domain::LedgerRow {
        id: domain::LedgerId::new(),
        media_id: media.id,
        path: strm_file.display().to_string(),
        season: None,
        episode: None,
        resolution: Some("2160p".into()),
        codec: None,
        hdr: None,
        quality_source: domain::QualitySource::Release,
        confidence: domain::Confidence::High,
        filter_score: None,
    };
    state.store().lock().insert_ledger(&row).unwrap();

    // 校验入账成功
    let rows = state.store().lock().list_ledger().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, strm_file.display().to_string());

    // 2. 模拟用户直接删除了整部剧/电影目录（包含 strm）
    std::fs::remove_dir_all(&movie_dir).unwrap();

    // 3. 文件系统监控捕获到 movie_dir 的删除事件
    let tracker = Arc::new(StrmGraceTracker::new());
    let events = vec![DebouncedEvent {
        path: movie_dir.clone(),
        kind: notify_debouncer_mini::DebouncedEventKind::Any,
    }];

    // 调用事件处理，并推进到 45 秒宽限期之后。
    api::fs_watcher::handle_fs_events(&state, &tracker, events);
    tracker.set_now(tracker.current_now() + 45);
    api::fs_watcher::handle_fs_events(&state, &tracker, Vec::new());

    // 4. 验证 ledger 中的该文件记录被标记缺失，而不是自动物理删除
    let remaining_rows = state.store().lock().list_ledger().unwrap();
    assert_eq!(
        remaining_rows.len(),
        1,
        "整目录缺失后，strm 记录应保留但被标记缺失"
    );
    assert!(
        state.store().lock().missing_at_by_path().unwrap().contains_key(&strm_file.display().to_string()),
        "文件应被标记缺失"
    );

    // 5. 验证 media 记录以及用户事实被保留
    let remaining_media = state.store().lock().get_media(media.id).unwrap();
    assert!(
        remaining_media.is_some(),
        "文件缺失不应删除 Media 用户事实"
    );
    // 6. 模拟从回收站还原该目录（重新创建目录与 strm 文件）
    std::fs::create_dir_all(&movie_dir).unwrap();
    std::fs::write(&strm_file, "https://example.com/video.mkv").unwrap();

    // 监控捕获到 movie_dir 的还原事件
    let restore_events = vec![DebouncedEvent {
        path: movie_dir.clone(),
        kind: notify_debouncer_mini::DebouncedEventKind::Any,
    }];
    api::fs_watcher::handle_fs_events(&state, &tracker, restore_events);

    // 等待异步 spawn_blocking 的扫描完成
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // 7. 验证还原后重新入账
    let restored_rows = state.store().lock().list_ledger().unwrap();
    assert_eq!(
        restored_rows.len(),
        1,
        "目录还原后应重新扫描入账，但未找到记录"
    );
    assert_eq!(restored_rows[0].path, strm_file.display().to_string());
}

fn episode_row(media_id: domain::MediaId, path: &std::path::Path, episode: u32) -> domain::LedgerRow {
    domain::LedgerRow {
        id: domain::LedgerId::new(),
        media_id,
        path: path.display().to_string(),
        season: Some(1),
        episode: Some(episode),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: domain::QualitySource::Release,
        confidence: domain::Confidence::High,
        filter_score: None,
    }
}

#[tokio::test]
async fn finder_and_recycle_metadata_events_are_ignored_before_handling() {
    let temp = tempfile::tempdir().unwrap();
    let library_root = temp.path().join("media/tv/china");
    let show_dir = library_root.join("喜剧之王 (2026)");
    let season_dir = show_dir.join("Season 1");
    std::fs::create_dir_all(&season_dir).unwrap();
    let episode = season_dir.join("喜剧之王 - S01E01 - 第 1 集.strm");
    std::fs::write(&episode, "https://example.com/e01.mkv").unwrap();
    let finder_metadata = season_dir.join(".DS_Store");
    let recycle_file = show_dir.join("#recycle/喜剧之王 - S01E01.strm");
    std::fs::create_dir_all(recycle_file.parent().unwrap()).unwrap();
    std::fs::write(&finder_metadata, "finder metadata").unwrap();
    std::fs::write(&recycle_file, "https://example.com/recycle.mkv").unwrap();

    let store = api::Store::open(temp.path().join("data")).unwrap();
    store
        .create_library(
            domain::MediaKind::Tv,
            "电视剧",
            &[library_root.to_str().unwrap()],
            "all",
            true,
            &[],
        )
        .unwrap();
    let state = api::management::ApiState::new(
        store,
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Tv,
        title: "喜剧之王".into(),
        year: Some(2026),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    state.store().lock().insert_media(&media).unwrap();
    let row = episode_row(media.id, &episode, 1);
    let ledger_id = row.id;
    state.store().lock().insert_ledger(&row).unwrap();

    let tracker = Arc::new(StrmGraceTracker::new());
    let events = vec![fs_event(finder_metadata), fs_event(recycle_file)];
    assert!(
        api::fs_watcher::relevant_fs_events(&events).is_empty(),
        ".DS_Store 和回收站事件必须在处理前过滤"
    );
    api::fs_watcher::handle_fs_events(&state, &tracker, events);

    assert!(
        state
            .store()
            .lock()
            .latest_probe_job_for_scope(&format!("ledger:{ledger_id}"))
            .unwrap()
            .is_none(),
        "被忽略的元数据变动不能让已有剧集重新探测"
    );
}

#[tokio::test]
async fn hidden_finder_metadata_does_not_reprobe_existing_episodes() {
    let temp = tempfile::tempdir().unwrap();
    let library_root = temp.path().join("media/tv/china");
    let show_dir = library_root.join("喜剧之王 (2026)");
    let season_dir = show_dir.join("Season 1");
    std::fs::create_dir_all(&season_dir).unwrap();
    let episode = season_dir.join("喜剧之王 - S01E01 - 第 1 集.strm");
    std::fs::write(&episode, "https://example.com/e01.mkv").unwrap();
    let finder_metadata = show_dir.join(".DS_Store");
    std::fs::write(&finder_metadata, "finder metadata").unwrap();

    let store = api::Store::open(temp.path().join("data")).unwrap();
    store
        .create_library(
            domain::MediaKind::Tv,
            "电视剧",
            &[library_root.to_str().unwrap()],
            "all",
            true,
            &[],
        )
        .unwrap();
    let state = api::management::ApiState::new(
        store,
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Tv,
        title: "喜剧之王".into(),
        year: Some(2026),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    state.store().lock().insert_media(&media).unwrap();
    let row = episode_row(media.id, &episode, 1);
    let ledger_id = row.id;
    state.store().lock().insert_ledger(&row).unwrap();

    let tracker = Arc::new(StrmGraceTracker::new());
    api::fs_watcher::handle_fs_events(&state, &tracker, vec![fs_event(finder_metadata)]);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    assert!(
        state
            .store()
            .lock()
            .latest_probe_job_for_scope(&format!("ledger:{ledger_id}"))
            .unwrap()
            .is_none(),
        ".DS_Store 变动不能让已有剧集重新探测或提取声纹"
    );
}

fn fs_event(path: std::path::PathBuf) -> DebouncedEvent {
    DebouncedEvent {
        path,
        kind: notify_debouncer_mini::DebouncedEventKind::Any,
    }
}

#[tokio::test]
async fn deleting_one_episode_keeps_sibling_ledger_rows() {
    let temp = tempfile::tempdir().unwrap();
    let library_root = temp.path().join("media/tv/china");
    let season_dir = library_root.join("喜剧之王 (2026)/Season 1");
    std::fs::create_dir_all(&season_dir).unwrap();
    let episode_one = season_dir.join("喜剧之王 - S01E01 - 第 1 集.strm");
    let episode_two = season_dir.join("喜剧之王 - S01E02 - 第 2 集.strm");
    std::fs::write(&episode_one, "https://example.com/e01.mkv").unwrap();
    std::fs::write(&episode_two, "https://example.com/e02.mkv").unwrap();

    let store = api::Store::open(temp.path().join("data")).unwrap();
    store
        .create_library(
            domain::MediaKind::Tv,
            "电视剧",
            &[library_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let state = api::management::ApiState::new(
        store,
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Tv,
        title: "喜剧之王".into(),
        year: Some(2026),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    state.store().lock().insert_media(&media).unwrap();
    let first = episode_row(media.id, &episode_one, 1);
    let second = episode_row(media.id, &episode_two, 2);
    let first_id = first.id;
    state.store().lock().insert_ledger(&first).unwrap();
    state.store().lock().insert_ledger(&second).unwrap();

    std::fs::remove_file(&episode_two).unwrap();
    // macOS 的 notify 会在单集删除后上报父目录，且事件处理时 path.exists()
    // 可能为 false，即使目录随后仍在。只把季目录临时挪走，E01 留在原路径。
    let parked_season = season_dir.with_file_name("Season 1.parked");
    let episode_one_parked = parked_season.join(episode_one.file_name().unwrap());
    std::fs::rename(&season_dir, &parked_season).unwrap();
    std::fs::create_dir_all(episode_one.parent().unwrap()).unwrap();
    std::fs::rename(&episode_one_parked, &episode_one).unwrap();
    let tracker = Arc::new(StrmGraceTracker::new());
    api::fs_watcher::handle_fs_events(
        &state,
        &tracker,
        vec![fs_event(episode_two.clone()), fs_event(season_dir.clone())],
    );
    tracker.set_now(tracker.current_now() + 45);
    api::fs_watcher::handle_fs_events(&state, &tracker, Vec::new());
    if parked_season.exists() {
        let _ = std::fs::remove_dir_all(&parked_season);
    }

    let rows = state.store().lock().list_ledger().unwrap();
    assert_eq!(rows.len(), 2, "文件缺失后台账保留并被标记缺失: {rows:?}");
    assert_eq!(rows[0].id, first_id, "仍在磁盘上的 E01 不能换 ledger_id");
    assert_eq!(rows[0].path, episode_one.display().to_string());
    assert!(state.store().lock().missing_at_by_path().unwrap().contains_key(&episode_two.display().to_string()));
    assert!(!state.store().lock().missing_at_by_path().unwrap().contains_key(&episode_one.display().to_string()));
    assert!(state.store().lock().get_media(media.id).unwrap().is_some());
}

#[tokio::test]
async fn restoring_one_episode_within_grace_keeps_its_probe_caches() {
    let fixture = episode_fixture();
    let tracker = Arc::new(StrmGraceTracker::new());
    tracker.on_created_or_modified(&fixture.episode, Some("https://example.com/e02.mkv"));
    std::fs::remove_file(&fixture.episode).unwrap();
    api::fs_watcher::handle_fs_events(
        &fixture.state,
        &tracker,
        vec![fs_event(fixture.episode.clone())],
    );

    let store = fixture.state.store();
    let during_grace = store.lock();
    let row = during_grace
        .ledger_by_path(&fixture.episode.display().to_string())
        .unwrap()
        .expect("宽限期内台账应保留");
    assert_eq!(row.id, fixture.ledger_id);
    assert!(during_grace.get_file_meta(&row.id.to_string()).unwrap().is_some());
    assert!(
        during_grace
            .get_fingerprint_cache(&row.id.to_string())
            .unwrap()
            .is_some()
    );
    assert!(
        during_grace
            .get_media_marker(fixture.media_id, Some(1), Some(2))
            .unwrap()
            .is_some()
    );
    drop(during_grace);

    std::fs::write(&fixture.episode, "https://example.com/e02.mkv").unwrap();
    api::fs_watcher::handle_fs_events(
        &fixture.state,
        &tracker,
        vec![fs_event(fixture.episode.clone())],
    );

    let store = fixture.state.store();
    let after_restore = store.lock();
    let restored = after_restore
        .ledger_by_path(&fixture.episode.display().to_string())
        .unwrap()
        .expect("相同 URL 恢复后仍用原来的台账");
    assert_eq!(restored.id, fixture.ledger_id);
    assert!(
        !after_restore
            .is_probe_queued(&restored.id.to_string())
            .unwrap(),
        "宽限期内相同 URL 恢复不应重新探测"
    );
}

#[tokio::test]
async fn restoring_one_episode_after_grace_requeues_probe() {
    let fixture = episode_fixture();
    let tracker = Arc::new(StrmGraceTracker::new());
    tracker.on_created_or_modified(&fixture.episode, Some("https://example.com/e02.mkv"));
    std::fs::remove_file(&fixture.episode).unwrap();
    api::fs_watcher::handle_fs_events(
        &fixture.state,
        &tracker,
        vec![fs_event(fixture.episode.clone())],
    );
    tracker.set_now(tracker.current_now() + 45);
    api::fs_watcher::handle_fs_events(&fixture.state, &tracker, Vec::new());

    let store = fixture.state.store();
    let after_expiry = store.lock();
    assert!(
        after_expiry
            .missing_at_by_path()
            .unwrap()
            .contains_key(&fixture.episode.display().to_string()),
        "宽限期过后这一集应被标记缺失"
    );
    assert!(
        after_expiry
            .get_media_marker(fixture.media_id, Some(1), Some(2))
            .unwrap()
            .is_some(),
        "标记缺失时不应删除片头片尾"
    );
    assert!(
        after_expiry
            .unit_state(fixture.user_id, fixture.media_id, 1, 2)
            .unwrap()
            .is_some(),
        "播放进度是用户数据，删集时保留"
    );
    assert!(after_expiry.get_media(fixture.media_id).unwrap().is_some());
    drop(after_expiry);

    std::fs::write(&fixture.episode, "https://example.com/e02.mkv").unwrap();
    api::fs_watcher::handle_fs_events(
        &fixture.state,
        &tracker,
        vec![fs_event(fixture.episode.clone())],
    );

    let restored = wait_for_restored_ledger(&fixture);
    assert_eq!(restored.id, fixture.ledger_id, "文件恢复时保留原有台账");
    let missing_cleared = !fixture
        .state
        .store()
        .lock()
        .missing_at_by_path()
        .unwrap()
        .contains_key(&fixture.episode.display().to_string());
    assert!(missing_cleared, "恢复后缺失标记应被清除");
}

fn wait_for_restored_ledger(fixture: &EpisodeFixture) -> domain::LedgerRow {
    for _ in 0..50 {
        if let Ok(Some(row)) = fixture
            .state
            .store()
            .lock()
            .ledger_by_path(&fixture.episode.display().to_string())
        {
            return row;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("过期后恢复应重新入账");
}

struct EpisodeFixture {
    state: api::management::ApiState,
    episode: std::path::PathBuf,
    media_id: domain::MediaId,
    ledger_id: domain::LedgerId,
    user_id: domain::UserId,
}

fn episode_fixture() -> EpisodeFixture {
    let temp = tempfile::tempdir().unwrap();
    let library_root = temp.path().join("media/tv/china");
    let season_dir = library_root.join("喜剧之王 (2026)/Season 1");
    std::fs::create_dir_all(&season_dir).unwrap();
    let episode = season_dir.join("喜剧之王 - S01E02 - 第 2 集.strm");
    let sibling = season_dir.join("喜剧之王 - S01E01 - 第 1 集.strm");
    std::fs::write(&episode, "https://example.com/e02.mkv").unwrap();
    std::fs::write(&sibling, "https://example.com/e01.mkv").unwrap();
    let store = api::Store::open(temp.path().join("data")).unwrap();
    store
        .create_library(
            domain::MediaKind::Tv,
            "电视剧",
            &[library_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let state = api::management::ApiState::new(
        store,
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();
    let media_id = domain::MediaId::new();
    state
        .store()
        .lock()
        .insert_media(&domain::Media {
            id: media_id,
            kind: domain::MediaKind::Tv,
            title: "喜剧之王".into(),
            year: Some(2026),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let ledger_id = domain::LedgerId::new();
    state
        .store()
        .lock()
        .insert_ledger(&domain::LedgerRow {
            id: ledger_id,
            media_id,
            path: episode.display().to_string(),
            season: Some(1),
            episode: Some(2),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Release,
            confidence: domain::Confidence::High,
            filter_score: None,
        })
        .unwrap();
    state
        .store()
        .lock()
        .insert_ledger(&domain::LedgerRow {
            id: domain::LedgerId::new(),
            media_id,
            path: sibling.display().to_string(),
            season: Some(1),
            episode: Some(1),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Release,
            confidence: domain::Confidence::High,
            filter_score: None,
        })
        .unwrap();
    let tracks = library::Tracks {
        video: Some(library::VideoTrack {
            codec: Some("h264".into()),
            width: Some(1920),
            height: Some(1080),
            ..library::VideoTrack::default()
        }),
        audio: Vec::new(),
        subtitles: Vec::new(),
    };
    state
        .store()
        .lock()
        .put_file_meta(&ledger_id.to_string(), &tracks)
        .unwrap();
    state
        .store()
        .lock()
        .put_fingerprint_cache(
            &ledger_id.to_string(),
            &store::FingerprintCacheEntry {
                cache_key: "episode-cache".into(),
                algorithm_version: 1,
                sample_duration_secs: 180,
                media_duration_ms: Some(2_500_000),
                intro: vec![1, 2, 3],
                outro: Some(vec![4, 5, 6]),
            },
        )
        .unwrap();
    state
        .store()
        .lock()
        .put_media_marker(&store::StoredMediaMarker {
            media_id,
            season: 1,
            episode: 2,
            intro_start_ms: Some(0),
            intro_end_ms: Some(90_000),
            outro_start_ms: Some(2_300_000),
            outro_end_ms: Some(2_400_000),
            source: "fingerprint".into(),
            locked: false,
            updated_at: 0,
        })
        .unwrap();
    let user_id = domain::UserId::new();
    state
        .store()
        .lock()
        .upsert_unit(user_id, media_id, 1, 2, 12_000, None, None, None, None, None, false, 1)
        .unwrap();
    std::mem::forget(temp);
    EpisodeFixture {
        state,
        episode,
        media_id,
        ledger_id,
        user_id,
    }
}

#[tokio::test]
async fn deleting_the_show_directory_removes_every_episode_ledger_row() {
    let temp = tempfile::tempdir().unwrap();
    let library_root = temp.path().join("media/tv/china");
    let show_dir = library_root.join("喜剧之王 (2026)");
    let season_dir = show_dir.join("Season 1");
    std::fs::create_dir_all(&season_dir).unwrap();
    let episode_one = season_dir.join("喜剧之王 - S01E01 - 第 1 集.strm");
    let episode_two = season_dir.join("喜剧之王 - S01E02 - 第 2 集.strm");
    std::fs::write(&episode_one, "https://example.com/e01.mkv").unwrap();
    std::fs::write(&episode_two, "https://example.com/e02.mkv").unwrap();

    let store = api::Store::open(temp.path().join("data")).unwrap();
    store
        .create_library(
            domain::MediaKind::Tv,
            "电视剧",
            &[library_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let state = api::management::ApiState::new(
        store,
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Tv,
        title: "喜剧之王".into(),
        year: Some(2026),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    state.store().lock().insert_media(&media).unwrap();
    state
        .store()
        .lock()
        .insert_ledger(&episode_row(media.id, &episode_one, 1))
        .unwrap();
    state
        .store()
        .lock()
        .insert_ledger(&episode_row(media.id, &episode_two, 2))
        .unwrap();

    std::fs::remove_dir_all(&show_dir).unwrap();
    let tracker = Arc::new(StrmGraceTracker::new());
    api::fs_watcher::handle_fs_events(&state, &tracker, vec![fs_event(show_dir)]);
    tracker.set_now(tracker.current_now() + 45);
    api::fs_watcher::handle_fs_events(&state, &tracker, Vec::new());

    assert_eq!(
        state.store().lock().missing_at_by_path().unwrap().len(),
        2,
        "整目录删除后两集台账都应被标记缺失"
    );
    assert!(state.store().lock().get_media(media.id).unwrap().is_some());
}
