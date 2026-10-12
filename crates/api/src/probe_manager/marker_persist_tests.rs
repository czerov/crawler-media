#[test]
fn fresh_fingerprint_replaces_stale_unlocked_intro_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media_id = domain::MediaId::new();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "万神殿".into(),
            year: None,
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: tmp.path().join("Pantheon.S01E01.mkv").display().to_string(),
        season: Some(1),
        episode: Some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    std::fs::write(&row.path, b"fixture").unwrap();
    store.insert_ledger(&row).unwrap();
    store
        .put_media_marker(&crate::store::StoredMediaMarker {
            media_id,
            season: 1,
            episode: 1,
            intro_start_ms: Some(0),
            intro_end_ms: Some(104_000),
            outro_start_ms: None,
            outro_end_ms: None,
            source: "theintrodb".into(),
            locked: false,
            updated_at: 0,
        })
        .unwrap();

    persist_marker(
        &store,
        &[row.clone()],
        crate::store::StoredMediaMarker {
            media_id,
            season: 1,
            episode: 1,
            intro_start_ms: Some(0),
            intro_end_ms: Some(180_000),
            outro_start_ms: Some(1_000_000),
            outro_end_ms: Some(1_100_000),
            source: "fingerprint".into(),
            locked: false,
            updated_at: 0,
        },
        false,
    )
    .unwrap();

    let marker = store
        .get_media_marker(media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(marker.intro_end_ms, Some(180_000));
    assert_eq!(marker.outro_start_ms, Some(1_000_000));
    assert_eq!(marker.source, "fingerprint");
}

fn fingerprint_marker(media_id: domain::MediaId) -> crate::store::StoredMediaMarker {
    crate::store::StoredMediaMarker {
        media_id,
        season: 1,
        episode: 1,
        intro_start_ms: Some(0),
        intro_end_ms: Some(90_000),
        outro_start_ms: Some(1_000_000),
        outro_end_ms: Some(1_060_000),
        source: "fingerprint".into(),
        locked: false,
        updated_at: 0,
    }
}

fn tv_episode(tmp: &tempfile::TempDir) -> (Store, domain::MediaId, LedgerRow) {
    let store = Store::open(tmp.path()).unwrap();
    let media_id = domain::MediaId::new();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "喜剧之王".into(),
            year: Some(2026),
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let video = tmp.path().join("episode.strm");
    std::fs::write(&video, "https://cdn.example.test/episode.mkv\n").unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: video.display().to_string(),
        season: Some(1),
        episode: Some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).unwrap();
    (store, media_id, row)
}

/// 假 ffmpeg：把参数记到固定文件并写出一张空图。路径写死在脚本里，
/// 后台抓帧线程不读环境变量，并行测试也不会互相清掉。
fn recording_ffmpeg(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let ffmpeg = bin.join("ffmpeg");
    let args = dir.join("ffmpeg-args");
    std::fs::write(
        &ffmpeg,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nprintf '\\n---\\n' >> '{}'\nout=\nwhile [ $# -gt 0 ]; do out=\"$1\"; shift; done\n: > \"$out\"\nexit 0\n",
            args.display(),
            args.display(),
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (ffmpeg, args)
}

/// 后台章节抓帧工作的**完成同步点**：等 registry 上该 library 的 lease 全部归还。
///
/// 这里不用固定 sleep / 重试：`wait_idle` 由 lease drop 时的 `notify_waiters`
/// 唤醒，属于确定性同步；超时只是"真卡死时不要挂住测试"的失败检测，
/// 不是同步手段（负载下 2s 太紧会把正常的慢完成误判成失败，DB16）。
const WORK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn wait_for_idle(
    registry: &std::sync::Arc<crate::library_lifecycle::LibraryWorkRegistry>,
    library_id: &str,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        tokio::time::timeout(WORK_TIMEOUT, registry.wait_idle(library_id))
            .await
            .expect("background chapter frame work did not finish");
    });
}

#[test]
fn fingerprint_marker_extracts_scene_frames_when_library_enables_them() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, media_id, row) = tv_episode(&tmp);
    let root = tmp.path().to_str().unwrap().to_string();
    let library = store
        .create_library(MediaKind::Tv, "国语剧", &[&root], "everyone", true, &[])
        .unwrap();
    store
        .set_library_switch_settings(&library.id, None, None, Some(true), None, None)
        .unwrap();

    let (ffmpeg, args) = recording_ffmpeg(tmp.path());
    crate::http::library_chapters::set_scene_frame_ffmpeg_for_test(leak_path(ffmpeg));
    let updates =
        persist_marker(&store, &[row.clone()], fingerprint_marker(media_id), false).unwrap();
    let store = std::sync::Arc::new(parking_lot::Mutex::new(store));
    let registry = std::sync::Arc::new(crate::library_lifecycle::LibraryWorkRegistry::default());
    crate::http::library_chapters::trigger_scene_frames_for_chapter_updates(
        &updates, &store, &registry,
    );
    // 必须等抓帧工作**做完**再读参数文件：`args` 由 shell 的 `>>` 重定向先建后写，
    // 只等"文件出现"会读到还没写入内容的空文件，断言就会假失败（DB16）。
    wait_for_idle(&registry, &library.id);
    let recorded = std::fs::read_to_string(&args).unwrap_or_default();
    assert!(
        recorded.contains("-frames:v"),
        "开启生成章节后，声纹写入片头片尾必须后台抓场景图: {recorded}"
    );
}

#[test]
fn fingerprint_marker_skips_scene_frames_when_library_disables_them() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, media_id, row) = tv_episode(&tmp);
    let root = tmp.path().to_str().unwrap().to_string();
    let library = store
        .create_library(MediaKind::Tv, "国语剧", &[&root], "everyone", true, &[])
        .unwrap();
    store
        .set_library_switch_settings(&library.id, None, None, Some(false), None, None)
        .unwrap();

    let (ffmpeg, args) = recording_ffmpeg(tmp.path());
    crate::http::library_chapters::set_scene_frame_ffmpeg_for_test(leak_path(ffmpeg));
    let updates =
        persist_marker(&store, &[row.clone()], fingerprint_marker(media_id), false).unwrap();
    let store = std::sync::Arc::new(parking_lot::Mutex::new(store));
    let registry = std::sync::Arc::new(crate::library_lifecycle::LibraryWorkRegistry::default());
    crate::http::library_chapters::trigger_scene_frames_for_chapter_updates(
        &updates, &store, &registry,
    );
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        !args.is_file(),
        "关闭生成章节后，声纹写入片头片尾不得抓场景图"
    );
    let marker = store
        .lock()
        .get_media_marker(media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(marker.source, "fingerprint");
}

#[tokio::test]
async fn cancelling_detached_chapter_frames_reaps_child_and_preserves_sidecars() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, media_id, row) = tv_episode(&tmp);
    let root = tmp.path().to_str().unwrap().to_string();
    let owner = store
        .create_library(MediaKind::Tv, "Extra", &[&root], "everyone", true, &[])
        .unwrap();
    store
        .set_library_switch_settings(&owner.id, None, None, Some(true), None, None)
        .unwrap();
    let started = tmp.path().join("frame-started");
    let ffmpeg = tmp.path().join("blocking-ffmpeg");
    std::fs::write(
        &ffmpeg,
        format!(
            "#!/bin/sh\nprintf ready > '{}'\nwhile :; do :; done\n",
            started.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    crate::http::library_chapters::set_scene_frame_ffmpeg_for_test(leak_path(ffmpeg));
    let updates =
        persist_marker(&store, &[row.clone()], fingerprint_marker(media_id), false).unwrap();
    let store = std::sync::Arc::new(parking_lot::Mutex::new(store));
    let registry = std::sync::Arc::new(crate::library_lifecycle::LibraryWorkRegistry::default());
    crate::http::library_chapters::trigger_scene_frames_for_chapter_updates(
        &updates, &store, &registry,
    );
    // 条件等待（子进程写出 ready 文件）+ 明确超时；不用固定 sleep 当同步手段。
    tokio::time::timeout(WORK_TIMEOUT, async {
        while !started.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fake ffmpeg did not start");
    registry.freeze(&store, &owner.id).unwrap();
    tokio::time::timeout(WORK_TIMEOUT, registry.wait_idle(&owner.id))
        .await
        .expect("chapter frame work did not stop after freeze");
    let image = std::path::Path::new(&row.path).with_file_name("episode-chapter_0.jpg");
    assert!(!image.exists());
    assert_eq!(
        std::fs::read_to_string(&row.path).unwrap(),
        "https://cdn.example.test/episode.mkv\n"
    );
    assert!(!std::fs::read_dir(tmp.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".chapter-")
    }));
}

#[test]
fn stale_marker_snapshot_does_not_resurrect_deleted_episode() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, media_id, row) = tv_episode(&tmp);
    store.delete_ledger_path(&row.path).unwrap();
    let updates = persist_marker(&store, &[row.clone()], fingerprint_marker(media_id), false).unwrap();
    assert!(updates.is_empty());
    assert!(store.get_media_marker(media_id, Some(1), Some(1)).unwrap().is_none());
    assert!(store.get_cached_chapters(&row.id.to_string()).unwrap().is_none());
}

#[test]
fn marker_snapshot_cannot_follow_reimported_path_to_a_new_ledger_id() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, media_id, row) = tv_episode(&tmp);
    store.delete_ledger_path(&row.path).unwrap();
    let mut reimported = row.clone();
    reimported.id = LedgerId::new();
    store.insert_ledger(&reimported).unwrap();
    let updates = persist_marker(&store, &[row.clone()], fingerprint_marker(media_id), false).unwrap();
    assert!(updates.is_empty());
    assert!(store.get_media_marker(media_id, Some(1), Some(1)).unwrap().is_none());
    assert!(store.get_cached_chapters(&reimported.id.to_string()).unwrap().is_none());
}

#[test]
fn frozen_marker_snapshot_does_not_publish_any_result() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, media_id, row) = tv_episode(&tmp);
    let owner = store.create_library(
        MediaKind::Tv, "Frozen", &[tmp.path().to_str().unwrap()], "everyone", true, &[],
    ).unwrap();
    store.begin_library_removal(&owner.id).unwrap();
    let updates = persist_marker(&store, &[row.clone()], fingerprint_marker(media_id), false).unwrap();
    assert!(updates.is_empty());
    assert!(store.get_media_marker(media_id, Some(1), Some(1)).unwrap().is_none());
    assert!(store.get_cached_chapters(&row.id.to_string()).unwrap().is_none());
    assert!(std::path::Path::new(&row.path).exists());
}

#[test]
fn marker_publication_skips_frozen_library_but_updates_live_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, media_id, row) = tv_episode(&tmp);
    let frozen = store.create_library(
        MediaKind::Tv, "Frozen", &[tmp.path().to_str().unwrap()], "everyone", true, &[],
    ).unwrap();
    let other = tempfile::tempdir().unwrap();
    store.create_library(
        MediaKind::Tv, "Live", &[other.path().to_str().unwrap()], "everyone", true, &[],
    ).unwrap();
    let mut live = row.clone();
    live.id = LedgerId::new();
    live.path = other.path().join("episode.strm").display().to_string();
    std::fs::write(&live.path, "https://fixture.invalid/episode.mkv").unwrap();
    store.insert_ledger(&live).unwrap();
    store.begin_library_removal(&frozen.id).unwrap();
    let updates = persist_marker(
        &store, &[row.clone(), live.clone()], fingerprint_marker(media_id), false,
    ).unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].0, live.id.to_string());
    assert!(store.get_cached_chapters(&row.id.to_string()).unwrap().is_none());
    assert!(store.get_cached_chapters(&live.id.to_string()).unwrap().is_some());
}

fn leak_path(path: std::path::PathBuf) -> &'static str {
    Box::leak(path.display().to_string().into_boxed_str())
}
