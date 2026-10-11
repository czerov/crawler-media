//! `Store::app_write` must roll back every app.db write when the callback
//! fails, and commit them when it succeeds.

use store::{Store, StoreError};

const KEY: &str = "app_write.fixture";

#[test]
fn app_write_rolls_back_when_the_callback_fails() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.put_setting(KEY, "old").unwrap();

    let result = store.app_write(|s| {
        s.put_setting(KEY, "new")?;
        Err::<(), _>(StoreError::Missing("fixture".into()))
    });

    assert!(matches!(result, Err(StoreError::Missing(_))));
    drop(store);

    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.get_setting(KEY).unwrap().as_deref(), Some("old"));
}

#[test]
fn app_write_commits_when_the_callback_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.put_setting(KEY, "old").unwrap();

    store
        .app_write(|s| s.put_setting(KEY, "new"))
        .unwrap();
    drop(store);

    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.get_setting(KEY).unwrap().as_deref(), Some("new"));
}
