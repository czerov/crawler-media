//! Regression tests for TV metadata refresh sidecars when directory was renamed
//! but stale ledger rows still linger during grace period.

mod fixture;
use fixture::RenameFixture;

#[tokio::test]
async fn rename_grace_flat_sidecars() {
    let f = RenameFixture::new("");
    f.refresh().await;
    f.assert_live_sidecars("");
}

#[tokio::test]
async fn rename_grace_season_layout() {
    let f = RenameFixture::new("Season 01");
    f.refresh().await;
    f.assert_live_sidecars("Season 01");
}

#[tokio::test]
async fn rename_grace_all_missing_does_not_recreate_directory() {
    let f = RenameFixture::new("");
    // Delete the live root as well, so all files on disk are gone
    std::fs::remove_dir_all(&f.live_root).unwrap();
    f.refresh().await;
    assert!(!f.old_root.exists(), "old root must not be recreated");
    assert!(!f.live_root.exists(), "live root must not be recreated if all files missing");
    let store_guard = f.state.store();
    let store = store_guard.lock();
    assert_eq!(store.ledger_for_media(f.media.id).unwrap().len(), 4, "ledgers must remain during grace");
    assert!(store.get_media(f.media.id).unwrap().is_some());
}

#[tokio::test]
async fn rename_grace_stale_preferred_row_does_not_poison_target() {
    let f = RenameFixture::new("");
    // Insert an episode 99 pointing to the old root, which makes it higher season/episode
    // than real live files.
    let old_e99 = f.old_root.join("Spirit.Rangers.S01E99.strm");
    let store_guard = f.state.store();
    let store = store_guard.lock();
    store.insert_ledger(&domain::LedgerRow {
        id: domain::LedgerId::new(),
        media_id: f.media.id,
        path: old_e99.display().to_string(),
        season: Some(1),
        episode: Some(99),
        resolution: Some("1080p".into()),
        codec: None,
        hdr: None,
        quality_source: domain::QualitySource::Release,
        confidence: domain::Confidence::High,
        filter_score: None,
    }).unwrap();
    drop(store);
    drop(store_guard);

    f.refresh().await;
    // With E99, total ledgers is 5
    assert!(f.live_root.join("tvshow.nfo").is_file());
    assert!(f.live_root.join("season.nfo").is_file());
    for episode in 1..=2 {
        let stem = format!("Spirit.Rangers.S01E{episode:02}");
        assert!(f.live_root.join(format!("{stem}.nfo")).is_file());
        assert_eq!(
            std::fs::read(f.live_root.join(format!("{stem}-thumb.jpg"))).unwrap(),
            if episode == 1 { b"one" } else { b"two" }
        );
    }
    assert!(!f.old_root.exists(), "old root must not be recreated even with stale preferred row");
    let store_guard = f.state.store();
    let store = store_guard.lock();
    assert_eq!(store.ledger_for_media(f.media.id).unwrap().len(), 5);
}
