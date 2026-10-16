use super::fixture::{TransferFixture, torrent};

#[tokio::test]
async fn mixed_tv_retry_binds_old_subtitle_to_persisted_custom_destination() {
    let fixture = TransferFixture::new(true, "{title}/{season}/{episode}{ext}").await;
    let torrent = torrent("The.Long.Watch.S01E01-E02.1080p", "mixed-tv");
    let e1 = fixture.source("The.Long.Watch.S01E01.1080p.mkv", b"episode-one");
    let sub = fixture.source("The.Long.Watch.S01E01.1080p.zh.srt", b"subtitle-one");
    let e2 = fixture.source("The.Long.Watch.S01E02.1080p.mkv", b"episode-two");
    let d1 = fixture.destination("tv/The Long Watch/01/01.mkv");
    let d2 = fixture.destination("tv/The Long Watch/01/02.mkv");
    let blocker = d1.with_extension("zh.srt");
    std::fs::create_dir_all(&blocker).unwrap();
    fixture.files(&torrent, &[e1.clone(), sub.clone()]);
    fixture.pending(&torrent);

    assert!(
        fixture.transfer().await.is_err(),
        "Subtitle failure must remain visible"
    );
    assert_eq!(std::fs::read(&d1).unwrap(), b"episode-one");
    assert!(!d2.exists());
    fixture.assert_sources(&[(&e1, &d1)]);
    fixture.assert_pending(1, 0);
    let original_id = fixture.state.store().lock().list_ledger().unwrap()[0].id;
    fixture.unblock(&blocker, "tv/The Long Watch/01/01.zh.srt");

    // Downloader still returns E1: ownership dedup must not discard its source identity.
    fixture.files(&torrent, &[e2.clone(), sub, e1.clone()]);
    assert!(fixture.transfer().await.is_ok());
    assert_eq!(std::fs::read(&blocker).unwrap(), b"subtitle-one");
    assert_eq!(std::fs::read(&d2).unwrap(), b"episode-two");
    assert!(
        !d2.with_extension("zh.srt").exists(),
        "E1 subtitle must not bind to newly imported E2"
    );
    fixture.assert_sources(&[(&e1, &d1), (&e2, &d2)]);
    fixture.assert_pending(0, 1);
    let rows = fixture.state.store().lock().list_ledger().unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.path == d1.to_str().unwrap())
            .unwrap()
            .id,
        original_id
    );
    assert!(fixture.transfer().await.is_ok());
    fixture.assert_sources(&[(&e1, &d1), (&e2, &d2)]);
}

#[tokio::test]
async fn all_owned_videos_without_subtitles_complete_reactivated_pending() {
    let fixture = TransferFixture::new(true, "{title}/{season}/{episode}{ext}").await;
    let torrent = torrent("The.Long.Watch.S01E01.1080p", "owned-tv");
    let source = fixture.source("The.Long.Watch.S01E01.1080p.mkv", b"owned-video");
    let destination = fixture.destination("tv/The Long Watch/01/01.mkv");
    fixture.files(&torrent, std::slice::from_ref(&source));
    fixture.pending(&torrent);
    assert!(fixture.transfer().await.is_ok());
    fixture.assert_pending(0, 1);
    let original_id = fixture.state.store().lock().list_ledger().unwrap()[0].id;

    fixture.pending(&torrent);
    fixture.assert_pending(1, 0);
    assert!(fixture.transfer().await.is_ok());
    fixture.assert_pending(0, 1);
    fixture.assert_sources(&[(&source, &destination)]);
    assert_eq!(
        fixture.state.store().lock().list_ledger().unwrap()[0].id,
        original_id
    );
    assert_eq!(std::fs::read(destination).unwrap(), b"owned-video");
}

#[tokio::test]
async fn user_deleted_destination_is_not_restored_or_falsely_marked_imported() {
    let fixture = TransferFixture::new(true, "{title}/{season}/{episode}{ext}").await;
    let torrent = torrent("The.Long.Watch.S01E01.1080p", "deleted-tv");
    let source = fixture.source("The.Long.Watch.S01E01.1080p.mkv", b"deleted-video");
    let sub = fixture.source("The.Long.Watch.S01E01.1080p.zh.srt", b"subtitle");
    let destination = fixture.destination("tv/The Long Watch/01/01.mkv");
    let blocker = destination.with_extension("zh.srt");
    std::fs::create_dir_all(&blocker).unwrap();
    fixture.files(&torrent, &[source.clone(), sub]);
    fixture.pending(&torrent);
    assert!(fixture.transfer().await.is_err());
    fixture.assert_pending(1, 0);
    fixture.assert_sources(&[(&source, &destination)]);
    let expected = fixture
        .tmp
        .path()
        .canonicalize()
        .unwrap()
        .join("tv/The Long Watch/01/01.mkv");
    assert_eq!(destination.canonicalize().unwrap(), expected);
    std::fs::remove_file(&destination).unwrap();
    fixture.unblock(&blocker, "tv/The Long Watch/01/01.zh.srt");

    let result = fixture.transfer().await;
    assert!(
        !destination.exists(),
        "Retry must respect user-deleted video: {result:?}"
    );
    assert!(
        !blocker.exists(),
        "A missing video must not gain an orphan subtitle"
    );
    fixture.assert_pending(1, 0);
    fixture.assert_sources(&[(&source, &destination)]);
}

#[tokio::test]
async fn unrelated_torrent_with_same_episode_does_not_steal_owned_destination() {
    let fixture = TransferFixture::new(true, "{title}/{season}/{episode}{ext}").await;
    let owned = torrent("The.Long.Watch.S01E01.1080p", "owned-version");
    let own_source = fixture.source("owned/The.Long.Watch.S01E01.1080p.mkv", b"owned");
    let destination = fixture.destination("tv/The Long Watch/01/01.mkv");
    fixture.files(&owned, std::slice::from_ref(&own_source));
    fixture.pending(&owned);
    assert!(fixture.transfer().await.is_ok());
    fixture.assert_pending(0, 1);

    let unrelated = torrent("The.Long.Watch.S01E01.1080p", "unrelated-version");
    let other_source = fixture.source("unrelated/The.Long.Watch.S01E01.1080p.mkv", b"unrelated");
    let other_sub = fixture.source(
        "unrelated/The.Long.Watch.S01E01.1080p.zh.srt",
        b"wrong-subtitle",
    );
    fixture.files(&unrelated, &[other_source.clone(), other_sub]);
    fixture.pending(&unrelated);
    let result = fixture.transfer().await;

    // A different source is a parallel version: it must never overwrite the owned
    // row, and its subtitle must not bind to the owned destination.
    assert!(
        !destination.with_extension("zh.srt").exists(),
        "Unrelated source must not bind by episode facts: {result:?}"
    );
    assert_eq!(std::fs::read(&destination).unwrap(), b"owned");
    let rows = fixture
        .state
        .store()
        .lock()
        .list_ledger_with_mode()
        .unwrap();
    let owned_row = rows
        .iter()
        .find(|(row, _, _)| row.path == destination.to_str().unwrap())
        .unwrap();
    assert_eq!(owned_row.2.as_deref(), own_source.to_str());
    // M2: without wash-cut the episode is already owned, so the unrelated
    // source is a duplicate and is not imported (no parallel library copy).
    assert_eq!(rows.len(), 1, "owned episode is not duplicated (M2)");
    fixture.assert_pending(0, 2);
}

#[tokio::test]
async fn parallel_versions_same_episode_retry_subtitles_against_exact_source() {
    let fixture = TransferFixture::new(true, "{title}/{season}/{episode}-{resolution}{ext}").await;
    let torrent = torrent("The.Long.Watch.S01E01.1080p", "parallel-tv");
    let low = fixture.source("The.Long.Watch.S01E01.1080p.mkv", b"low-video");
    let high = fixture.source("The.Long.Watch.S01E01.2160p.mkv", b"high-video");
    let low_sub = fixture.source("The.Long.Watch.S01E01.1080p.zh.srt", b"low-subtitle");
    let high_sub = fixture.source("The.Long.Watch.S01E01.2160p.zh.srt", b"high-subtitle");
    let low_dest = fixture.destination("tv/The Long Watch/01/01-1080p.mkv");
    let high_dest = fixture.destination("tv/The Long Watch/01/01-2160p.mkv");
    let low_blocker = low_dest.with_extension("zh.srt");
    let high_blocker = high_dest.with_extension("zh.srt");
    std::fs::create_dir_all(&low_blocker).unwrap();
    std::fs::create_dir_all(&high_blocker).unwrap();
    fixture.files(
        &torrent,
        &[low.clone(), high.clone(), high_sub.clone(), low_sub.clone()],
    );
    fixture.pending(&torrent);
    assert!(fixture.transfer().await.is_err());
    fixture.assert_sources(&[(&low, &low_dest), (&high, &high_dest)]);
    fixture.assert_pending(1, 0);
    fixture.unblock(&low_blocker, "tv/The Long Watch/01/01-1080p.zh.srt");
    fixture.unblock(&high_blocker, "tv/The Long Watch/01/01-2160p.zh.srt");
    fixture.files(&torrent, &[high.clone(), low.clone(), low_sub, high_sub]);

    assert!(fixture.transfer().await.is_ok());
    assert_eq!(std::fs::read(low_blocker).unwrap(), b"low-subtitle");
    assert_eq!(std::fs::read(high_blocker).unwrap(), b"high-subtitle");
    fixture.assert_sources(&[(&low, &low_dest), (&high, &high_dest)]);
    fixture.assert_pending(0, 1);
}
