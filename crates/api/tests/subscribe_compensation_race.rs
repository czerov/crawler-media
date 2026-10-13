use domain::MediaId;
use store::Store;

#[test]
fn undo_ensured_media_guards_and_leaves_referenced_media() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let media_id = MediaId::new();
    let media = domain::Media {
        id: media_id,
        kind: domain::MediaKind::Movie,
        title: "Test Movie".into(),
        year: Some(2024),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();

    // With no reference, undo succeeds
    assert!(store.get_media(media_id).unwrap().is_some());
    let deleted = store.undo_ensured_media_if_unreferenced(media_id, &media).unwrap();
    assert!(deleted);
    assert!(store.get_media(media_id).unwrap().is_none());
}
