//! Regression tests for TV metadata refresh sidecars when directory was renamed
//! but stale ledger rows still linger during grace period.

mod fixture;
use fixture::RenameFixture;
use serde_json::json;
use super::common::request;
use tower::ServiceExt;

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

#[tokio::test]
async fn rename_grace_two_seasons() {
    let f = RenameFixture::new("two_seasons");
    f.refresh().await;
    assert!(f.live_root.join("tvshow.nfo").is_file());
    for season in 1..=2 {
        let dir = f.live_root.join(format!("Season {season:02}"));
        assert!(dir.join("season.nfo").is_file());
        assert!(!dir.join("tvshow.nfo").exists());
        for episode in 1..=2 {
            let stem = format!("Spirit.Rangers.S{season:02}E{episode:02}");
            let nfo = std::fs::read_to_string(dir.join(format!("{stem}.nfo"))).unwrap();
            assert!(nfo.contains("Episode plot"));
            assert_eq!(
                std::fs::read(dir.join(format!("{stem}-thumb.jpg"))).unwrap(),
                if episode == 1 { &b"one"[..] } else { &b"two"[..] }
            );
        }
    }
    assert!(!f.old_root.exists());
}

#[tokio::test]
async fn rename_grace_keeps_two_live_copies_separate() {
    let f = RenameFixture::new("");
    let copy = f.live_root.parent().unwrap().join("Spirit Rangers Copy");
    std::fs::create_dir_all(&copy).unwrap();
    let store_guard = f.state.store();
    let store = store_guard.lock();
    for episode in 1..=2 {
        let path = copy.join(format!("Spirit.Rangers.S01E{episode:02}.strm"));
        std::fs::write(&path, b"https://example.com/copy").unwrap();
        store
            .insert_ledger(&domain::LedgerRow {
                id: domain::LedgerId::new(),
                media_id: f.media.id,
                path: path.display().to_string(),
                season: Some(1),
                episode: Some(episode),
                resolution: Some("1080p".into()),
                codec: None,
                hdr: None,
                quality_source: domain::QualitySource::Release,
                confidence: domain::Confidence::High,
                filter_score: None,
            })
            .unwrap();
    }
    drop(store);
    drop(store_guard);
    f.refresh().await;
    assert!(f.live_root.join("tvshow.nfo").is_file());
    assert!(copy.join("tvshow.nfo").is_file());
    assert!(f.live_root.join("season.nfo").is_file());
    assert!(copy.join("season.nfo").is_file());
    assert!(!copy.join("Season 01").exists());
    assert!(!f.old_root.exists());
}

#[tokio::test]
async fn rename_grace_without_nfo_still_writes_live_stills() {
    let f = RenameFixture::new("");
    let response = f
        .app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/settings/scrape",
            Some("management-secret"),
            json!({"setting": {"mirror_nfo": false, "mirror_episode_thumbs": true}}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    f.refresh().await;
    assert!(!f.live_root.join("tvshow.nfo").exists());
    assert!(!f.live_root.join("season.nfo").exists());
    for episode in 1..=2 {
        let stem = format!("Spirit.Rangers.S01E{episode:02}");
        assert!(!f.live_root.join(format!("{stem}.nfo")).exists());
        assert_eq!(
            std::fs::read(f.live_root.join(format!("{stem}-thumb.jpg"))).unwrap(),
            if episode == 1 { &b"one"[..] } else { &b"two"[..] }
        );
    }
    assert!(!f.old_root.exists());
}

#[tokio::test]
async fn rename_grace_preserves_existing_episode_still() {
    let f = RenameFixture::new("");
    let still = f.live_root.join("Spirit.Rangers.S01E01-thumb.jpg");
    std::fs::write(&still, b"kept").unwrap();
    f.refresh().await;
    assert_eq!(std::fs::read(&still).unwrap(), b"kept");
    assert_eq!(
        std::fs::read(f.live_root.join("Spirit.Rangers.S01E02-thumb.jpg")).unwrap(),
        b"two"
    );
    assert!(!f.old_root.exists());
}
