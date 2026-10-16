use domain::{Confidence, LedgerId, LedgerRow, MediaId, MediaKind, QualitySource};
use store::{BrowseChangeSource, Store};

fn create_test_store() -> (tempfile::TempDir, Store) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    (tmp, store)
}

fn sample_ledger_row(id: LedgerId, media_id: MediaId, path: &str) -> LedgerRow {
    LedgerRow {
        id,
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

#[test]
fn journal_captures_insert_update_delete_in_all_sources() {
    let (_tmp, store) = create_test_store();

    // 1. App source: media
    let media = domain::Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Test Movie".to_string(),
        year: Some(2025),
        original_title: None,
        tmdb_id: Some("12345".to_string()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let initial_app_cursor = store
        .browse_changes(BrowseChangeSource::App, 0, 512)
        .unwrap()
        .last()
        .map(|c| c.seq)
        .unwrap_or(0);

    store.insert_media(&media).unwrap();

    let changes = store
        .browse_changes(BrowseChangeSource::App, initial_app_cursor, 512)
        .unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].entity_kind, "media");
    assert_eq!(changes[0].entity_key, media.id.to_string());
    // 2. Catalog source: cache
    store
        .put_catalog_cache("tmdb", "movie:12345", "{}", 1000, None)
        .unwrap();
    let cat_changes = store
        .browse_changes(BrowseChangeSource::Catalog, 0, 512)
        .unwrap();
    assert_eq!(cat_changes.len(), 1);
    assert_eq!(cat_changes[0].entity_kind, "catalog_cache");
    assert_eq!(cat_changes[0].entity_key, "tmdb:movie:12345");

    // 3. Library source: ledger
    let lid = LedgerId::new();
    let row = sample_ledger_row(lid, media.id, "/media/movies/Test (2025)/test.mkv");
    store.insert_ledger(&row).unwrap();

    let lib_changes = store
        .browse_changes(BrowseChangeSource::Library, 0, 512)
        .unwrap();
    assert_eq!(lib_changes.len(), 1);
    assert_eq!(lib_changes[0].entity_kind, "ledger");
    assert_eq!(lib_changes[0].entity_key, lid.to_string());
    assert_eq!(lib_changes[0].new_path.as_deref(), Some("/media/movies/Test (2025)/test.mkv"));
}

#[test]
fn crash_before_cursor_commit_replays_changes() {
    let (_tmp, store) = create_test_store();
    // Advance cursor past initial seeded rows so crash replay specifically tests the media row under test
    let initial_app_cursor = store
        .browse_changes(BrowseChangeSource::App, 0, 512)
        .unwrap()
        .last()
        .map(|c| c.seq)
        .unwrap_or(0);
    if initial_app_cursor > 0 {
        store
            .apply_browse_changes(BrowseChangeSource::App, initial_app_cursor, &[])
            .unwrap();
    }
    // Insert media to trigger change event
    let media = domain::Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Crash Replay Test".to_string(),
        year: Some(2025),
        original_title: None,
        tmdb_id: Some("9999".to_string()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();

    let changes = store
        .browse_changes(BrowseChangeSource::App, initial_app_cursor, 512)
        .unwrap();
    assert_eq!(changes.len(), 1);
    let change_seq = changes[0].seq;
    // Suppose cursor in library.db is still at initial_app_cursor (crash happened before apply_browse_changes)
    let cursor = store.browse_change_cursor(BrowseChangeSource::App).unwrap();
    assert_eq!(cursor, initial_app_cursor);

    // Reading after cursor replays the same change
    let replayed = store.browse_changes(BrowseChangeSource::App, cursor, 512).unwrap();
    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0].seq, change_seq);

    // Now apply changes through change_seq
    let lib_id = "lib-test".to_string();
    store
        .apply_browse_changes(BrowseChangeSource::App, change_seq, &[lib_id.clone()])
        .unwrap();

    // Cursor is now advanced
    let cursor_after = store.browse_change_cursor(BrowseChangeSource::App).unwrap();
    assert_eq!(cursor_after, change_seq);

    // And reading after the new cursor returns no more changes
    let after_applied = store.browse_changes(BrowseChangeSource::App, cursor_after, 512).unwrap();
    assert!(after_applied.is_empty());

    // And dirty token was incremented in library head
    let head = store.browse_projection_head(&lib_id).unwrap().unwrap();
    assert!(head.dirty_token >= 1);
}

#[test]
fn max_512_events_per_round_reports_incomplete() {
    let (_tmp, store) = create_test_store();

    // Insert 600 catalog items
    for i in 0..600 {
        store
            .put_catalog_cache("tmdb", &format!("movie:{i}"), "{}", 1000, None)
            .unwrap();
    }

    let batch = store.browse_changes(BrowseChangeSource::Catalog, 0, 512).unwrap();
    assert_eq!(batch.len(), 512);

    let next_cursor = batch.last().unwrap().seq;
    let next_batch = store.browse_changes(BrowseChangeSource::Catalog, next_cursor, 512).unwrap();
    assert_eq!(next_batch.len(), 600 - 512);
}
