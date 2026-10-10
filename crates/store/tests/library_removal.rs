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
