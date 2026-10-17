use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, UserId};
use rusqlite::Connection;
use store::Store;

fn ledger_row(media_id: MediaId, path: &str) -> LedgerRow {
    LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: path.to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    }
}

fn sample_media(id: MediaId) -> Media {
    Media {
        id,
        kind: MediaKind::Movie,
        title: "Test Movie".to_string(),
        original_title: None,
        aliases: Vec::new(),
        year: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

#[test]
fn deleting_ledger_removes_only_file_derived_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();

    let media_id1 = MediaId::new();
    let media1 = sample_media(media_id1);
    store.insert_media(&media1).unwrap();

    let media_id2 = MediaId::new();
    let media2 = sample_media(media_id2);
    store.insert_media(&media2).unwrap();

    let row1 = ledger_row(media_id1, "/movies/one.mkv");
    let row2 = ledger_row(media_id2, "/movies/two.mkv");
    store.insert_ledger(&row1).unwrap();
    store.insert_ledger(&row2).unwrap();

    // Populate file_meta directly in library.db
    let lib_conn = Connection::open(data_dir.join("library.db")).unwrap();
    lib_conn
        .execute(
            "INSERT INTO file_meta (ledger_id, audio_json, subtitle_json, video_json, tracks_version, format_duration_ms)
             VALUES (?1, '[]', '[]', '{}', 2, 120000)",
            [&row1.id.to_string()],
        )
        .unwrap();
    lib_conn
        .execute(
            "INSERT INTO file_meta (ledger_id, audio_json, subtitle_json, video_json, tracks_version, format_duration_ms)
             VALUES (?1, '[]', '[]', '{}', 2, 120000)",
            [&row2.id.to_string()],
        )
        .unwrap();

    // Insert scrape_sidecars directly in library.db
    lib_conn
        .execute(
            "INSERT INTO scrape_sidecars (ledger_id, kind, path) VALUES (?1, 'nfo', '/movies/one.nfo')",
            [&row1.id.to_string()],
        )
        .unwrap();
    lib_conn
        .execute(
            "INSERT INTO scrape_sidecars (ledger_id, kind, path) VALUES (?1, 'nfo', '/movies/two.nfo')",
            [&row2.id.to_string()],
        )
        .unwrap();

    // Set playback for two users
    let user1 = UserId::new();
    let user2 = UserId::new();
    store
        .set_playback_state(user1, media_id1, 500, Some(true), None, None, None)
        .unwrap();
    store
        .set_playback_state(user2, media_id1, 800, Some(false), None, None, None)
        .unwrap();

    // Delete row1
    store.delete_ledger_path(&row1.path).unwrap();

    // Verify row1 is gone, but media1, user playbacks, and row2 sidecars/meta are preserved
    assert!(store.get_ledger(&row1.id.to_string()).unwrap().is_none());
    assert!(
        store
            .get_file_meta(&row1.id.to_string())
            .unwrap()
            .is_none()
    );

    let sidecar_count1: i64 = lib_conn
        .query_row(
            "SELECT COUNT(*) FROM scrape_sidecars WHERE ledger_id = ?1",
            [&row1.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sidecar_count1, 0, "sidecar for row1 must be deleted from db");

    // Row2 preserved
    assert!(store.get_ledger(&row2.id.to_string()).unwrap().is_some());
    assert!(
        store
            .get_file_meta(&row2.id.to_string())
            .unwrap()
            .is_some()
    );
    let sidecar_count2: i64 = lib_conn
        .query_row(
            "SELECT COUNT(*) FROM scrape_sidecars WHERE ledger_id = ?1",
            [&row2.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sidecar_count2, 1, "sidecar for row2 must remain");

    // Media and user facts preserved
    assert!(store.get_media(media_id1).unwrap().is_some());
    assert_eq!(
        store.playback_progress(user1, media_id1).unwrap(),
        Some(500)
    );
    assert_eq!(
        store.playback_progress(user2, media_id1).unwrap(),
        Some(800)
    );
}

#[test]
fn repeated_delete_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let media_id = MediaId::new();
    store.insert_media(&sample_media(media_id)).unwrap();
    let row = ledger_row(media_id, "/movies/test.mkv");
    store.insert_ledger(&row).unwrap();

    store.delete_ledger_path(&row.path).unwrap();
    // Second delete should succeed and be a no-op
    store.delete_ledger_path(&row.path).unwrap();
}

#[test]
fn derived_delete_failure_rolls_back_ledger_and_caches() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();

    let media_id = MediaId::new();
    store.insert_media(&sample_media(media_id)).unwrap();
    let row = ledger_row(media_id, "/movies/test.mkv");
    store.insert_ledger(&row).unwrap();

    let lib_conn = Connection::open(data_dir.join("library.db")).unwrap();
    lib_conn
        .execute(
            "INSERT INTO file_meta (ledger_id, audio_json, subtitle_json, video_json, tracks_version, format_duration_ms)
             VALUES (?1, '[]', '[]', '{}', 2, 60000)",
            [&row.id.to_string()],
        )
        .unwrap();
    lib_conn
        .execute(
            "INSERT INTO scrape_sidecars (ledger_id, kind, path) VALUES (?1, 'nfo', '/movies/test.nfo')",
            [&row.id.to_string()],
        )
        .unwrap();

    // Trigger on library.db that fails when deleting from scrape_sidecars
    lib_conn
        .execute(
            "CREATE TRIGGER fail_sidecar_del BEFORE DELETE ON scrape_sidecars
             BEGIN
                 SELECT RAISE(ABORT, 'forced trigger abort');
             END;",
            [],
        )
        .unwrap();

    let res = store.delete_ledger_path(&row.path);
    assert!(res.is_err(), "delete must fail due to trigger");

    // Check that ledger and file_meta were rolled back and still exist
    assert!(store.get_ledger(&row.id.to_string()).unwrap().is_some());
    assert!(
        store
            .get_file_meta(&row.id.to_string())
            .unwrap()
            .is_some()
    );
}

#[test]
fn media_delete_uses_same_transaction_cleanup() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();

    let media_id = MediaId::new();
    store.insert_media(&sample_media(media_id)).unwrap();
    let row1 = ledger_row(media_id, "/movies/part1.mkv");
    let row2 = ledger_row(media_id, "/movies/part2.mkv");
    store.insert_ledger(&row1).unwrap();
    store.insert_ledger(&row2).unwrap();

    let lib_conn = Connection::open(data_dir.join("library.db")).unwrap();
    lib_conn
        .execute(
            "INSERT INTO scrape_sidecars (ledger_id, kind, path) VALUES (?1, 'nfo', '/movies/part1.nfo')",
            [&row1.id.to_string()],
        )
        .unwrap();
    lib_conn
        .execute(
            "INSERT INTO scrape_sidecars (ledger_id, kind, path) VALUES (?1, 'nfo', '/movies/part2.nfo')",
            [&row2.id.to_string()],
        )
        .unwrap();

    let count = store.delete_ledger_for_media(media_id).unwrap();
    assert_eq!(count, 2);

    assert!(store.get_ledger(&row1.id.to_string()).unwrap().is_none());
    assert!(store.get_ledger(&row2.id.to_string()).unwrap().is_none());

    let sidecar_count1: i64 = lib_conn
        .query_row(
            "SELECT COUNT(*) FROM scrape_sidecars WHERE ledger_id = ?1",
            [&row1.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sidecar_count1, 0);

    let sidecar_count2: i64 = lib_conn
        .query_row(
            "SELECT COUNT(*) FROM scrape_sidecars WHERE ledger_id = ?1",
            [&row2.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sidecar_count2, 0);

    // Media itself must remain
    assert!(store.get_media(media_id).unwrap().is_some());
}
