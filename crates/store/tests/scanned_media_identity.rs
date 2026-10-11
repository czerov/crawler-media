use domain::{Media, MediaId, MediaKind};
use store::{Store, StoreError};
fn media(title: &str, tmdb: Option<&str>) -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: title.into(),
        year: Some(2022),
        original_title: None,
        tmdb_id: tmdb.map(str::to_owned),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}
#[test]
fn external_alias_reuses_identity_despite_title_change_and_keeps_kind_separate() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let old = media("Old title", Some("1"));
    store.insert_media(&old).unwrap();
    assert_eq!(
        store
            .resolve_scanned_media(media("新标题", Some("1")))
            .unwrap()
            .id,
        old.id
    );
    let mut movie = media("新标题", Some("1"));
    movie.kind = MediaKind::Movie;
    assert_ne!(store.resolve_scanned_media(movie).unwrap().id, old.id);
}
#[test]
fn conflicting_aliases_fail_without_mutating_existing_rows() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let first = media("Same", Some("1"));
    let mut second = media("Other", Some("2"));
    second.tvdb_id = Some("9".into());
    store.insert_media(&first).unwrap();
    store.insert_media(&second).unwrap();
    let mut incoming = media("Same", Some("1"));
    incoming.tvdb_id = Some("9".into());
    assert!(matches!(
        store.resolve_scanned_media(incoming),
        Err(StoreError::Protected(_))
    ));
    assert_eq!(store.get_media(first.id).unwrap().unwrap(), first);
    assert_eq!(store.get_media(second.id).unwrap().unwrap(), second);
    let different = store
        .resolve_scanned_media(media("Same", Some("3")))
        .unwrap();
    assert_ne!(different.id, first.id);
}
#[test]
fn duplicate_alias_and_single_row_alias_contradictions_are_protected() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let mut old = media("First", Some("1"));
    old.tvdb_id = Some("7".into());
    store.insert_media(&old).unwrap();
    let mut incoming = media("Renamed", Some("1"));
    incoming.tvdb_id = Some("8".into());
    assert!(matches!(
        store.resolve_scanned_media(incoming),
        Err(StoreError::Protected(_))
    ));
    store.insert_media(&media("Second", Some("1"))).unwrap();
    assert!(matches!(
        store.resolve_scanned_media(media("Third", Some("1"))),
        Err(StoreError::Protected(_))
    ));
    assert_eq!(store.get_media(old.id).unwrap().unwrap(), old);
}

#[test]
fn title_only_recovery_requires_unique_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let old = media("Same", Some("1"));
    store.insert_media(&old).unwrap();
    assert_eq!(
        store.resolve_scanned_media(media("Same", None)).unwrap().id,
        old.id
    );
    store.insert_media(&media("Same", Some("2"))).unwrap();
    assert!(matches!(
        store.resolve_scanned_media(media("Same", None)),
        Err(StoreError::Protected(_))
    ));
}
