use api::Store;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};

#[test]
fn more_specific_root_wins_even_after_ambiguous_parent_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let parent = tmp.path().join("media");
    let child = parent.join("tv");
    let parent_path = parent.to_str().unwrap();
    let child_path = child.to_str().unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "parent-a",
            &[parent_path],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "parent-b",
            &[parent_path],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let expected = store
        .create_library(MediaKind::Tv, "child", &[child_path], "everyone", true, &[])
        .unwrap();

    let owner = store
        .library_for_path(&child.join("episode.mkv"), MediaKind::Tv)
        .unwrap()
        .unwrap();
    assert_eq!(owner.id, expected.id);
}

#[test]
fn restricted_root_cannot_move_while_owned_ledger_rows_remain() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let old_root = tmp.path().join("private-old");
    let new_root = tmp.path().join("private-new");
    let restricted = store
        .create_library(
            MediaKind::Movie,
            "restricted",
            &[old_root.to_str().unwrap()],
            "selected",
            true,
            &[],
        )
        .unwrap();
    let path = old_root.join("secret.mkv");
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "secret".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: media.id,
            path: path.display().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();
    assert_eq!(
        store
            .library_for_path(&path, MediaKind::Movie)
            .unwrap()
            .unwrap()
            .id,
        restricted.id
    );

    let update = store.update_library(
        &restricted.id,
        None,
        Some(&[new_root.to_str().unwrap()]),
        None,
    );
    assert!(matches!(update, Err(api::store::StoreError::Protected(_))));

    assert_eq!(
        store
            .library_for_path(&path, MediaKind::Movie)
            .unwrap()
            .unwrap()
            .id,
        restricted.id,
        "the failed root update must preserve the row's restricted owner"
    );
}

#[tokio::test]
async fn deleting_library_with_owned_ledger_is_blocked_with_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let restricted_root = tmp.path().join("restricted");
    std::fs::create_dir_all(&restricted_root).unwrap();

    let restricted = store
        .create_library(
            MediaKind::Movie,
            "Restricted",
            &[restricted_root.to_str().unwrap()],
            "selected",
            false,
            &[],
        )
        .unwrap();

    let media_id = domain::MediaId::new();
    let path = restricted_root.join("secret.mkv");
    store
        .insert_ledger(&LedgerRow {
            id: domain::LedgerId::new(),
            media_id,
            path: path.to_str().unwrap().into(),
            season: None,
            episode: None,
            resolution: Some("1080p".into()),
            codec: Some("hevc".into()),
            hdr: None,
            quality_source: domain::QualitySource::Probe,
            confidence: domain::Confidence::High,
            filter_score: None,
        })
        .unwrap();

    // 尝试删除仍有台账文件的媒体库：必须返回 409 Conflict 或 Protected Error
    let err = store.delete_library(&restricted.id);
    assert!(
        matches!(err, Err(api::store::StoreError::Protected(_))),
        "仍有台账文件的媒体库必须禁止直接删除，防止文件失去受限保护回退到所有人可见的默认库"
    );
}
