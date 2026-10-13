use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use store::{
    FilePresence, LedgerPresenceMode, LedgerPresenceObservation, LedgerPresenceResult, Store,
};

fn sample_ledger(path: &str, media_id: MediaId) -> LedgerRow {
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
        year: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

#[test]
fn stale_observation_does_not_delete_replacement_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let media_id = MediaId::new();
    store.insert_media(&sample_media(media_id)).unwrap();

    let old_row = sample_ledger("/movies/one.mkv", media_id);
    store.insert_ledger(&old_row).unwrap();
    // Simulate it being marked missing
    let obs_missing = LedgerPresenceObservation {
        row: old_row.clone(),
        missing_at: None,
        presence: FilePresence::Missing,
    };
    store
        .apply_ledger_presence(&[obs_missing], LedgerPresenceMode::Verify, 1000)
        .unwrap();

    // Now imagine the old row was replaced by a new row at the same path!
    store.delete_ledger_path(&old_row.path).unwrap();
    let new_row = sample_ledger("/movies/one.mkv", media_id);
    store.insert_ledger(&new_row).unwrap();

    // A stale prune observation holding old_row
    let stale_prune_obs = LedgerPresenceObservation {
        row: old_row,
        missing_at: Some(1000),
        presence: FilePresence::Missing,
    };

    let result = store
        .apply_ledger_presence(&[stale_prune_obs], LedgerPresenceMode::Prune, 1050)
        .unwrap();

    assert_eq!(result.deleted, 0);
    assert_eq!(result.skipped, 1);
    // New ledger must still exist
    assert!(store.get_ledger(&new_row.id.to_string()).unwrap().is_some());
}

#[test]
fn restored_file_clears_flag_without_replacing_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let media_id = MediaId::new();
    store.insert_media(&sample_media(media_id)).unwrap();
    let row = sample_ledger("/movies/two.mkv", media_id);
    store.insert_ledger(&row).unwrap();

    // Verify missing
    let obs_missing = LedgerPresenceObservation {
        row: row.clone(),
        missing_at: None,
        presence: FilePresence::Missing,
    };
    store
        .apply_ledger_presence(&[obs_missing], LedgerPresenceMode::Verify, 1000)
        .unwrap();
    assert_eq!(
        store.missing_at_by_path().unwrap().get(&row.path).copied(),
        Some(1000)
    );

    // Now file is restored (Present)
    let obs_present = LedgerPresenceObservation {
        row: row.clone(),
        missing_at: Some(1000),
        presence: FilePresence::Present,
    };
    let res = store
        .apply_ledger_presence(&[obs_present], LedgerPresenceMode::Verify, 2000)
        .unwrap();
    assert_eq!(res.restored, 1);
    assert_eq!(res.missing, 0);

    // missing_at is cleared
    assert_eq!(
        store.missing_at_by_path().unwrap().get(&row.path).copied(),
        None
    );
    // Row identity and all fields unchanged
    let fetched = store.get_ledger(&row.id.to_string()).unwrap().unwrap();
    assert_eq!(fetched.id, row.id);
    assert_eq!(fetched.path, row.path);
    assert_eq!(fetched.media_id, row.media_id);
}

#[test]
fn unknown_observation_preserves_prior_state() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let media_id = MediaId::new();
    store.insert_media(&sample_media(media_id)).unwrap();
    let row = sample_ledger("/movies/three.mkv", media_id);
    store.insert_ledger(&row).unwrap();

    // Verify missing first
    let obs_missing = LedgerPresenceObservation {
        row: row.clone(),
        missing_at: None,
        presence: FilePresence::Missing,
    };
    store
        .apply_ledger_presence(&[obs_missing], LedgerPresenceMode::Verify, 1000)
        .unwrap();

    // Now an Unknown observation (e.g. PermissionDenied)
    let obs_unknown = LedgerPresenceObservation {
        row: row.clone(),
        missing_at: Some(1000),
        presence: FilePresence::Unknown,
    };
    let res = store
        .apply_ledger_presence(&[obs_unknown], LedgerPresenceMode::Verify, 2000)
        .unwrap();
    assert_eq!(res.skipped, 1);
    assert_eq!(res.missing, 0);
    assert_eq!(res.restored, 0);

    // Prior missing_at is preserved
    assert_eq!(
        store.missing_at_by_path().unwrap().get(&row.path).copied(),
        Some(1000)
    );
}

#[test]
fn prune_rolls_back_whole_batch_on_derived_error() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();

    let media_id = MediaId::new();
    store.insert_media(&sample_media(media_id)).unwrap();
    let row1 = sample_ledger("/movies/p1.mkv", media_id);
    let row2 = sample_ledger("/movies/p2.mkv", media_id);
    store.insert_ledger(&row1).unwrap();
    store.insert_ledger(&row2).unwrap();

    // Mark both missing
    let obs = vec![
        LedgerPresenceObservation {
            row: row1.clone(),
            missing_at: None,
            presence: FilePresence::Missing,
        },
        LedgerPresenceObservation {
            row: row2.clone(),
            missing_at: None,
            presence: FilePresence::Missing,
        },
    ];
    store
        .apply_ledger_presence(&obs, LedgerPresenceMode::Verify, 1000)
        .unwrap();

    // Trigger on library.db that fails on row2 deletion
    let lib_conn = rusqlite::Connection::open(data_dir.join("library.db")).unwrap();
    lib_conn
        .execute(
            "CREATE TRIGGER fail_row2_del BEFORE DELETE ON ledger
             FOR EACH ROW WHEN OLD.path = '/movies/p2.mkv'
             BEGIN
                 SELECT RAISE(ABORT, 'forced trigger abort on row2');
             END;",
            [],
        )
        .unwrap();

    let prune_obs = vec![
        LedgerPresenceObservation {
            row: row1.clone(),
            missing_at: Some(1000),
            presence: FilePresence::Missing,
        },
        LedgerPresenceObservation {
            row: row2.clone(),
            missing_at: Some(1000),
            presence: FilePresence::Missing,
        },
    ];

    let res = store.apply_ledger_presence(&prune_obs, LedgerPresenceMode::Prune, 2000);
    assert!(res.is_err(), "batch prune must fail due to trigger");

    // Both row1 and row2 must be preserved due to transaction rollback
    assert!(store.get_ledger(&row1.id.to_string()).unwrap().is_some());
    assert!(store.get_ledger(&row2.id.to_string()).unwrap().is_some());
}
