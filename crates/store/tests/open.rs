use store::Store;

#[test]
fn open_creates_four_sqlite_files() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    for name in ["app.db", "catalog.db", "library.db", "subscribe.db"] {
        assert!(tmp.path().join(name).is_file(), "{name} should exist");
    }
    let versions = store.schema_versions();
    assert_eq!(
        versions.iter().find(|(n, _)| *n == "app").unwrap().1,
        5,
        "app schema version should be 5"
    );
    assert_eq!(
        versions.iter().find(|(n, _)| *n == "catalog").unwrap().1,
        1,
        "catalog schema version should be 1"
    );
    assert_eq!(
        versions.iter().find(|(n, _)| *n == "library").unwrap().1,
        7,
        "library schema version should be 7"
    );
    assert_eq!(
        versions.iter().find(|(n, _)| *n == "subscribe").unwrap().1,
        4,
        "subscribe schema version should be 4"
    );
}
