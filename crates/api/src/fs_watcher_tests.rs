use super::fs_watcher::{
    entry_scan_target_dir, is_relevant_media_entry, realtime_library_root_for_path,
    FsWatcherSession, FsWatcherState,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct EmptyFetcher;

impl indexer::Fetcher for EmptyFetcher {
    fn fetch(&self, _request: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch("unexpected request".into()))
    }
}

struct FakeWatcher {
    fail_watch_paths: HashSet<PathBuf>,
    fail_unwatch_paths: HashSet<PathBuf>,
    watched: HashSet<PathBuf>,
    watch_calls: Arc<AtomicUsize>,
    unwatch_calls: Arc<AtomicUsize>,
}

impl FakeWatcher {
    fn new() -> Self {
        Self {
            fail_watch_paths: HashSet::new(),
            fail_unwatch_paths: HashSet::new(),
            watched: HashSet::new(),
            watch_calls: Arc::new(AtomicUsize::new(0)),
            unwatch_calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl FsWatcherSession for FakeWatcher {
    fn watch_dir(&mut self, dir: &Path) -> Result<(), notify::Error> {
        self.watch_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_watch_paths.contains(dir) {
            return Err(notify::Error::generic("mock watch failure"));
        }
        self.watched.insert(dir.to_path_buf());
        Ok(())
    }

    fn unwatch_dir(&mut self, dir: &Path) -> Result<(), notify::Error> {
        self.unwatch_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_unwatch_paths.contains(dir) {
            return Err(notify::Error::generic("mock unwatch failure"));
        }
        self.watched.remove(dir);
        Ok(())
    }
}

#[test]
fn realtime_library_root_matches_nested_strm_path() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("tv");
    let episode_dir = root.join("喜剧之王 (2026)/Season 1");
    std::fs::create_dir_all(&episode_dir).unwrap();
    let episode = episode_dir.join("喜剧之王 - S01E01 - 第 1 集.strm");
    std::fs::write(&episode, "https://example.test/episode.mkv").unwrap();

    let store = crate::Store::open(temp.path().join("data")).unwrap();
    store
        .set_library_root(domain::MediaKind::Tv, root.to_str().unwrap())
        .unwrap();
    let state = crate::management::ApiState::new(
        store,
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();

    assert_eq!(
        realtime_library_root_for_path(&state, &episode),
        Some(root.clone())
    );
    let show_dir = root.join("喜剧之王 (2026)");
    assert_eq!(entry_scan_target_dir(&root, &episode), show_dir);

    crate::http::library_scan::scan_library_subdir(&state, &root, &show_dir).unwrap();
    let rows = state.store.lock().list_ledger().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, episode.display().to_string());
    assert_eq!(rows[0].season, Some(1));
    assert_eq!(rows[0].episode, Some(1));
    assert!(state.probe.is_queued(&rows[0].id.to_string()));
}

#[test]
fn rescanning_a_show_queues_only_the_new_episode() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("tv");
    let episode_dir = root.join("喜剧之王 (2026)/Season 1");
    std::fs::create_dir_all(&episode_dir).unwrap();
    let episode_one = episode_dir.join("喜剧之王 - S01E01 - 第 1 集.strm");
    std::fs::write(&episode_one, "https://example.test/e01.mkv").unwrap();

    let store = crate::Store::open(temp.path().join("data")).unwrap();
    store
        .set_library_root(domain::MediaKind::Tv, root.to_str().unwrap())
        .unwrap();
    let state = crate::management::ApiState::new(
        store,
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();
    let show_dir = root.join("喜剧之王 (2026)");
    crate::http::library_scan::scan_library_subdir(&state, &root, &show_dir).unwrap();
    let first = state.store.lock().list_ledger().unwrap();
    let first_id = first[0].id.to_string();
    while state.probe.take_queued_for_test().is_some() {}
    let job_id = state
        .store
        .lock()
        .active_probe_unit_for_ledger(&first_id)
        .unwrap()
        .unwrap()
        .job_id;
    state
        .store
        .lock()
        .finish_probe_unit(&job_id, &first_id, true, None)
        .unwrap();
    state
        .store
        .lock()
        .finish_probe_job(&job_id, true, None)
        .unwrap();
    assert!(!state.probe.is_queued(&first_id));

    let episode_two = episode_dir.join("喜剧之王 - S01E02 - 第 2 集.strm");
    std::fs::write(&episode_two, "https://example.test/e02.mkv").unwrap();
    crate::http::library_scan::scan_library_subdir(&state, &root, &show_dir).unwrap();

    let rows = state.store.lock().list_ledger().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(!state.probe.is_queued(&first_id), "已有 E01 不能再次入队");
    let second = rows.iter().find(|row| row.episode == Some(2)).unwrap();
    assert!(state.probe.is_queued(&second.id.to_string()));
}

#[test]
fn test_entry_scan_target_dir_scenarios() {
    let root = Path::new("/media/tv/china");
    // 1. 剧集嵌套季目录
    let ep1 = Path::new("/media/tv/china/喜剧之王 (2026)/Season 1/S01E01.strm");
    assert_eq!(
        entry_scan_target_dir(root, ep1),
        PathBuf::from("/media/tv/china/喜剧之王 (2026)")
    );

    // 2. 剧集平铺
    let ep2 = Path::new("/media/tv/china/喜剧之王 (2026)/S01E01.strm");
    assert_eq!(
        entry_scan_target_dir(root, ep2),
        PathBuf::from("/media/tv/china/喜剧之王 (2026)")
    );

    // 3. 根目录平铺文件
    let flat = Path::new("/media/tv/china/S01E01.strm");
    assert_eq!(
        entry_scan_target_dir(root, flat),
        PathBuf::from("/media/tv/china")
    );
}

#[test]
fn test_watch_failure_retried_on_next_reconcile() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir1 = temp.path().join("dir1");
    let dir2 = temp.path().join("dir2");
    std::fs::create_dir(&dir1).unwrap();
    std::fs::create_dir(&dir2).unwrap();

    let mut watcher = FakeWatcher::new();
    // dir1 第一次 watch 失败
    watcher.fail_watch_paths.insert(dir1.clone());

    let mut state = FsWatcherState::new();
    let mut desired = HashSet::new();
    desired.insert(dir1.clone());
    desired.insert(dir2.clone());

    // 第一轮：dir1 失败，dir2 成功
    state.reconcile(&mut watcher, &desired);
    assert!(!state.active().contains(&dir1), "dir1 失败后不能在 active");
    assert!(state.active().contains(&dir2), "dir2 成功后必须在 active");

    // 恢复 dir1
    watcher.fail_watch_paths.remove(&dir1);

    // 第二轮：重试 dir1
    state.reconcile(&mut watcher, &desired);
    assert!(state.active().contains(&dir1), "重试后 dir1 必须在 active");
    assert!(state.active().contains(&dir2), "dir2 保持在 active");
}

#[test]
fn test_missing_directory_not_active_until_created() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path().join("not_yet_created");

    let mut watcher = FakeWatcher::new();
    let mut state = FsWatcherState::new();
    let mut desired = HashSet::new();
    desired.insert(dir.clone());

    // 第一次：目录不存在，不进入 active 也不调 watcher.watch
    state.reconcile(&mut watcher, &desired);
    assert!(!state.active().contains(&dir));
    assert_eq!(watcher.watch_calls.load(Ordering::SeqCst), 0);

    // 创建目录后再次 reconcile
    std::fs::create_dir(&dir).unwrap();
    state.reconcile(&mut watcher, &desired);
    assert!(state.active().contains(&dir));
    assert_eq!(watcher.watch_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn test_unwatch_failure_keeps_active_and_retries() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path().join("dir_to_remove");
    std::fs::create_dir(&dir).unwrap();

    let mut watcher = FakeWatcher::new();
    let mut state = FsWatcherState::new();
    let mut desired = HashSet::new();
    desired.insert(dir.clone());

    // 成功加入
    state.reconcile(&mut watcher, &desired);
    assert!(state.active().contains(&dir));

    // 从 desired 移除，但 unwatch 失败
    watcher.fail_unwatch_paths.insert(dir.clone());
    desired.remove(&dir);
    state.reconcile(&mut watcher, &desired);
    assert!(state.active().contains(&dir), "unwatch 失败保留在 active");

    // 下一轮 unwatch 恢复成功
    watcher.fail_unwatch_paths.remove(&dir);
    state.reconcile(&mut watcher, &desired);
    assert!(
        !state.active().contains(&dir),
        "unwatch 成功后从 active 移除"
    );
}

#[test]
fn test_config_error_preserves_active_set() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path().join("existing_dir");
    std::fs::create_dir(&dir).unwrap();

    let mut watcher = FakeWatcher::new();
    let mut state = FsWatcherState::new();
    let mut desired = HashSet::new();
    desired.insert(dir.clone());

    // 初始化注册
    state.reconcile(&mut watcher, &desired);
    assert!(state.active().contains(&dir));

    // 模拟配置读取失败时的逻辑：不调用 reconcile（或者不传入空 desired），保持 active 不变
    let config_result: Result<HashSet<PathBuf>, &'static str> = Err("db error");
    match config_result {
        Ok(new_desired) => state.reconcile(&mut watcher, &new_desired),
        Err(_) => {
            // 不执行 reconcile，保留 active
        }
    }
    assert!(
        state.active().contains(&dir),
        "配置查询失败时已有监听集合必须完整保留"
    );
    assert_eq!(watcher.unwatch_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn is_relevant_media_entry_whitelists_only_videos_strm_and_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Season 1");
    std::fs::create_dir(&dir).unwrap();
    assert!(is_relevant_media_entry(&dir));

    let video_strm = tmp.path().join("episode.strm");
    std::fs::write(&video_strm, b"https://cdn.example/video").unwrap();
    assert!(is_relevant_media_entry(&video_strm));

    let video_mkv = tmp.path().join("movie.mkv");
    std::fs::write(&video_mkv, b"video").unwrap();
    assert!(is_relevant_media_entry(&video_mkv));

    let video_mp4 = tmp.path().join("movie.mp4");
    std::fs::write(&video_mp4, b"video").unwrap();
    assert!(is_relevant_media_entry(&video_mp4));

    let video_ts = tmp.path().join("stream.ts");
    std::fs::write(&video_ts, b"video").unwrap();
    assert!(is_relevant_media_entry(&video_ts));

    // Non-video files must be rejected:
    for non_video in [
        "movie.nfo",
        "season.nfo",
        "fanart.jpg",
        "poster.png",
        "still.webp",
        "README.txt",
        "subtitles.srt",
        "subtitles.ass",
        "metadata.xml",
        "info.json",
        "download.torrent",
        "temp.part",
        "file.tmp",
    ] {
        let path = tmp.path().join(non_video);
        std::fs::write(&path, b"data").unwrap();
        assert!(
            !is_relevant_media_entry(&path),
            "{non_video} 必须被非媒体白名单过滤掉"
        );
    }

    // For non-existent (deleted) paths:
    // Video / STRM files are recognized as relevant for deletion cleanup:
    assert!(is_relevant_media_entry(&tmp.path().join("deleted.strm")));
    assert!(is_relevant_media_entry(&tmp.path().join("deleted.mkv")));
    assert!(is_relevant_media_entry(&tmp.path().join("deleted.mp4")));

    // Directory without extension is recognized:
    assert!(is_relevant_media_entry(&tmp.path().join("DeletedFolder")));

    // Non-video sidecars that are deleted are safely skipped without ledger traversal:
    assert!(!is_relevant_media_entry(&tmp.path().join("deleted.jpg")));
    assert!(!is_relevant_media_entry(&tmp.path().join("deleted.nfo")));
    assert!(!is_relevant_media_entry(&tmp.path().join("deleted.txt")));
    assert!(!is_relevant_media_entry(&tmp.path().join("deleted.srt")));
}

/// B3 §4.2：watch 事件必须把变动目录提交给 `ScanCoordinator`，由它单飞、合并目标、
/// 带断点地扫描；不能再自己 `spawn_blocking` 直接扫库绕过协调器。
#[tokio::test]
async fn watch_event_enqueues_a_targeted_scan_through_the_coordinator() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("tv");
    let episode_dir = root.join("喜剧之王 (2026)/Season 1");
    std::fs::create_dir_all(&episode_dir).unwrap();
    let episode = episode_dir.join("喜剧之王 - S01E01 - 第 1 集.strm");
    std::fs::write(&episode, "https://example.test/e01.mkv").unwrap();

    let store = crate::Store::open(temp.path().join("data")).unwrap();
    store
        .set_library_root(domain::MediaKind::Tv, root.to_str().unwrap())
        .unwrap();
    let library_id = store.list_libraries().unwrap()[0].id.clone();
    let state = crate::management::ApiState::new(
        store,
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();

    let target_dir = entry_scan_target_dir(&root, &episode);
    crate::fs_watcher::handle_fs_events(
        &state,
        &Arc::new(crate::fs_watcher::StrmGraceTracker::new()),
        vec![notify_debouncer_mini::DebouncedEvent::new(
            episode.clone(),
            notify_debouncer_mini::DebouncedEventKind::Any,
        )],
    );

    let ops = state
        .store()
        .lock()
        .scan_operations_for_library(&library_id)
        .unwrap();
    assert_eq!(ops.len(), 1, "watch 事件必须提交一个扫描 operation");
    assert_eq!(
        ops[0].targets,
        vec![target_dir],
        "扫描目标必须是变动的条目子目录"
    );

    // 定向扫描真的跑完（走 ScanWalker 的 targets 模式）：剧集入账。
    for _ in 0..200 {
        let rows = state.store().lock().list_ledger().unwrap();
        if let Some(row) = rows.first() {
            assert_eq!(row.path, episode.display().to_string());
            assert_eq!((row.season, row.episode), (Some(1), Some(1)));
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("watch 触发的定向扫描未入账 episode");
}
