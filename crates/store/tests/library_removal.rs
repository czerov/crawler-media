use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use store::Store;

fn media() -> Media {
    Media { id: MediaId::new(), kind: MediaKind::Tv, title: "Example".into(),
        year: Some(2022), original_title: None, tmdb_id: Some("123".into()),
        douban_id: None, tvdb_id: None, bangumi_id: None, anilist_id: None }
}

fn row(media_id: MediaId, path: &std::path::Path) -> LedgerRow {
    LedgerRow { id: LedgerId::new(), media_id, path: path.display().to_string(),
        season: Some(1), episode: Some(1), resolution: None, codec: None, hdr: None,
        quality_source: QualitySource::Release, confidence: Confidence::High, filter_score: None }
}

#[test]
fn removal_cleans_only_owned_rows_and_preserves_files_and_media() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let root = temp.path().join("tv_%");
    let child = root.join("kids");
    std::fs::create_dir_all(&child).unwrap();
    let a = store.create_library(MediaKind::Tv, "A", &[root.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let b = store.create_library(MediaKind::Tv, "B", &[child.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let media = media();
    store.insert_media(&media).unwrap();
    let path = root.join("E01.strm");
    let child_path = child.join("E01.strm");
    std::fs::write(&path, b"https://example.test/one").unwrap();
    std::fs::write(&child_path, b"https://example.test/two").unwrap();
    let a_row = row(media.id, &path);
    let b_row = row(media.id, &child_path);
    store.insert_ledger(&a_row).unwrap();
    store.insert_ledger(&b_row).unwrap();
    store.put_file_meta(&a_row.id.to_string(), &library::Tracks::default()).unwrap();
    let user = domain::UserId::new();
    store.upsert_unit(user, media.id, 1, 1, 750_000, Some(false), Some(true),
        Some(1_000_000), None, None, false, 1).unwrap();
    let removal = store.begin_library_removal(&a.id).unwrap();
    assert!(!store.library_accepts_work(&a.id).unwrap());
    assert_eq!(store.begin_library_removal(&a.id).unwrap().operation_id, removal.operation_id);
    assert!(store.clean_library_removal(&removal.operation_id).is_err());
    store.set_removal_state(&removal.operation_id, "cleaning", None).unwrap();
    let done = store.clean_library_removal(&removal.operation_id).unwrap();
    assert_eq!(done.state, "succeeded");
    assert_eq!(done.removed_ledger_count, 1);
    assert!(store.ledger_by_path(&a_row.path).unwrap().is_none());
    assert!(store.get_file_meta(&a_row.id.to_string()).unwrap().is_none());
    assert!(store.ledger_by_path(&b_row.path).unwrap().is_some());
    assert!(store.get_library(&b.id).unwrap().is_some());
    assert!(store.get_media(media.id).unwrap().is_some());
    let progress = store.unit_state(user, media.id, 1, 1).unwrap().unwrap();
    assert_eq!(progress.position_ms, 750_000);
    assert!(progress.favorite);
    assert_eq!(std::fs::read(&path).unwrap(), b"https://example.test/one");
    assert_eq!(store.clean_library_removal(&removal.operation_id).unwrap().removed_ledger_count, 1);
}

#[test]
fn removal_cleans_fingerprint_attempts_and_preserves_shared_season_comparison() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let root_a = temp.path().join("tv_a");
    let root_b = temp.path().join("tv_b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    let a = store.create_library(MediaKind::Tv, "A", &[root_a.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let _b = store.create_library(MediaKind::Tv, "B", &[root_b.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let media = media();
    store.insert_media(&media).unwrap();
    let path_a = root_a.join("E01.strm");
    let path_b = root_b.join("E01.strm");
    std::fs::write(&path_a, b"a").unwrap();
    std::fs::write(&path_b, b"b").unwrap();
    let a_row = row(media.id, &path_a);
    let b_row = row(media.id, &path_b);
    store.insert_ledger(&a_row).unwrap();
    store.insert_ledger(&b_row).unwrap();

    let attempt = store::StoredFingerprintAttempt {
        attempt_id: "att-1".into(),
        job_id: "job-1".into(),
        ledger_id: a_row.id.to_string(),
        kind: "intro".into(),
        window_start_ms: 0,
        window_end_ms: 10_000,
        phase: "capture".into(),
        started_at_ms: 1_000,
        finished_at_ms: None,
        status: "running".into(),
        error_kind: None,
        metrics_json: "{}".into(),
    };
    store.begin_fingerprint_attempt(&attempt).unwrap();
    assert!(store.get_fingerprint_attempt("att-1").unwrap().is_some());

    // 写入季对比历史：同一 Media、Season 1
    store.save_season_comparison_digest(&media.id.to_string(), 1, "digest-1", 1000).unwrap();

    let removal = store.begin_library_removal(&a.id).unwrap();
    store.set_removal_state(&removal.operation_id, "cleaning", None).unwrap();
    let done = store.clean_library_removal(&removal.operation_id).unwrap();
    assert_eq!(done.state, "succeeded");

    // a 库的 fingerprint_attempts 应被彻底删除
    assert!(store.get_fingerprint_attempt("att-1").unwrap().is_none());

    // 因为 b 库依然有同一个 Media 的 Season 1 ledger，所以该季的对比历史应该得以保留！
    assert!(store.season_comparison_digest(&media.id.to_string(), 1).unwrap().is_some());
}

#[test]
fn removal_closes_playback_sessions_for_removed_files_and_preserves_other_library() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let root_a = temp.path().join("tv_a");
    let root_b = temp.path().join("tv_b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    let a = store.create_library(MediaKind::Tv, "A", &[root_a.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let b = store.create_library(MediaKind::Tv, "B", &[root_b.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let media = media();
    store.insert_media(&media).unwrap();

    let path_a = root_a.join("E01.strm");
    let path_b = root_b.join("E01.strm");
    let a_row = row(media.id, &path_a);
    let b_row = row(media.id, &path_b);
    store.insert_ledger(&a_row).unwrap();
    store.insert_ledger(&b_row).unwrap();

    let user_a = domain::UserId::new();
    let user_b = domain::UserId::new();
    let device_a = "device-a";
    let device_b = "device-b";
    let session_a = store::SessionRow {
        device_id: device_a.into(),
        user_id: user_a,
        media_id: media.id,
        season: Some(1),
        episode: Some(1),
        client: Some("web".into()),
        device_name: Some("browser".into()),
        client_version: Some("1.0".into()),
        play_method: "direct".into(),
        position_ms: 10_000,
        start_position_ms: 0,
        duration_ms: Some(100_000),
        paused: false,
        watched_ms: 10_000,
        rate_bps: 0,
        bytes_sent: 0,
        connections: 1,
        admin_ended: false,
        started_at: 100,
        last_report_at: 200,
        ledger_id: Some(a_row.id.to_string()),
    };
    let session_b = store::SessionRow {
        device_id: device_b.into(),
        user_id: user_b,
        media_id: media.id,
        season: Some(1),
        episode: Some(1),
        client: Some("web".into()),
        device_name: Some("browser".into()),
        client_version: Some("1.0".into()),
        play_method: "direct".into(),
        position_ms: 50_000,
        start_position_ms: 0,
        duration_ms: Some(100_000),
        paused: false,
        watched_ms: 50_000,
        rate_bps: 0,
        bytes_sent: 0,
        connections: 1,
        admin_ended: false,
        started_at: 100,
        last_report_at: 200,
        ledger_id: Some(b_row.id.to_string()),
    };
    store.upsert_session(&session_a).unwrap();
    store.upsert_session(&session_b).unwrap();
    assert!(store.get_session(user_a, device_a).unwrap().is_some());
    assert!(store.get_session(user_b, device_b).unwrap().is_some());

    let removal = store.begin_library_removal(&a.id).unwrap();
    let closed = store.close_playback_sessions_for_removal(&removal.operation_id).unwrap();
    assert_eq!(closed, 1);

    // A 库的会话被精确关闭
    assert!(store.get_session(user_a, device_a).unwrap().is_none());
    // B 库的会话完好保留
    assert!(store.get_session(user_b, device_b).unwrap().is_some());
    assert_eq!(store.get_library(&b.id).unwrap().is_some(), true);
}
