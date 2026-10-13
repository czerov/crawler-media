#[path = "support/mod.rs"]
mod support;

use std::path::PathBuf;
use std::sync::Arc;
use notify_debouncer_mini::{DebouncedEvent, DebouncedEventKind};
use support::library_presence::PresenceFixture;

fn event(path: PathBuf) -> DebouncedEvent {
    DebouncedEvent {
        path,
        kind: DebouncedEventKind::Any,
    }
}

#[tokio::test]
async fn last_local_file_missing_keeps_media_and_all_user_facts() {
    let fix = PresenceFixture::new();
    let (media_id, row) = fix.insert_media_and_ledger("movie.mkv");

    // Add user playback fact
    let user_id = domain::UserId::new();
    {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard
            .set_playback_state(user_id, media_id, 5000, Some(true), Some(true), None, None)
            .unwrap();
    }

    // Trigger watcher missing event for the file
    let path = PathBuf::from(&row.path);
    let tracker = Arc::new(api::fs_watcher::StrmGraceTracker::default());
    api::fs_watcher::handle_fs_events(
        &fix.state,
        &tracker,
        vec![event(path.clone())],
    );

    // Ledger row still exists, but missing_at is set!
    let store_arc = fix.state.store();
    let guard = store_arc.lock();
    let loaded = guard.get_ledger(&row.id.to_string()).unwrap().unwrap();
    assert_eq!(loaded.id, row.id);
    assert!(guard.missing_at_by_path().unwrap().contains_key(&row.path));

    // Media and User facts PRESERVED (S1)
    assert!(guard.get_media(media_id).unwrap().is_some());
    assert_eq!(
        guard.playback_progress(user_id, media_id).unwrap(),
        Some(5000)
    );
}

#[tokio::test]
async fn last_strm_missing_after_45_seconds_keeps_ledger_and_history() {
    let fix = PresenceFixture::new();
    let (media_id, row) = fix.insert_media_and_ledger("sample.strm");

    let user_id = domain::UserId::new();
    {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard
            .set_playback_state(user_id, media_id, 2500, Some(false), Some(true), None, None)
            .unwrap();
    }

    let path = PathBuf::from(&row.path);
    let tracker = Arc::new(api::fs_watcher::StrmGraceTracker::default());
    tracker.set_now(1000);

    // Event at t = 1000
    api::fs_watcher::handle_fs_events(
        &fix.state,
        &tracker,
        vec![event(path.clone())],
    );

    // Advance time past 45 seconds (t = 1050) and trigger handle_fs_events
    tracker.set_now(1050);
    api::fs_watcher::handle_fs_events(&fix.state, &tracker, vec![]);

    // Ledger row and user facts MUST STILL EXIST!
    let store_arc = fix.state.store();
    let guard = store_arc.lock();
    assert!(guard.get_ledger(&row.id.to_string()).unwrap().is_some());
    assert!(guard.get_media(media_id).unwrap().is_some());
    assert_eq!(
        guard.playback_progress(user_id, media_id).unwrap(),
        Some(2500)
    );
}

#[tokio::test]
async fn same_url_restore_clears_missing_and_preserves_probe_cache() {
    let fix = PresenceFixture::new();
    let (_media_id, row) = fix.insert_media_and_ledger("video.strm");

    // Populate file_meta
    let lib_conn = rusqlite::Connection::open(fix.dir.path().join("data/library.db")).unwrap();
    lib_conn
        .execute(
            "INSERT INTO file_meta (ledger_id, audio_json, subtitle_json, video_json, tracks_version, format_duration_ms)
             VALUES (?1, '[]', '[]', '{}', 2, 80000)",
            [&row.id.to_string()],
        )
        .unwrap();

    // Mark missing
    let path = PathBuf::from(&row.path);
    let tracker = Arc::new(api::fs_watcher::StrmGraceTracker::default());
    tracker.on_created_or_modified(&path, Some("http://example.com/video.m3u8"));
    api::fs_watcher::handle_fs_events(
        &fix.state,
        &tracker,
        vec![event(path.clone())],
    );

    let store_arc = fix.state.store();
    assert!(
        store_arc
            .lock()
            .missing_at_by_path()
            .unwrap()
            .contains_key(&row.path)
    );

    // Write file back to disk with same content
    std::fs::write(&path, b"http://example.com/video.m3u8").unwrap();

    // Event present
    api::fs_watcher::handle_fs_events(
        &fix.state,
        &tracker,
        vec![event(path.clone())],
    );

    // missing_at is cleared
    assert!(
        !store_arc
            .lock()
            .missing_at_by_path()
            .unwrap()
            .contains_key(&row.path)
    );

    // file_meta is preserved
    assert!(
        store_arc
            .lock()
            .get_file_meta(&row.id.to_string())
            .unwrap()
            .is_some()
    );
}
