//! DB3：比程序更新的数据库必须拒绝降级打开，而且预检不得改动任何库。

use std::collections::HashMap;
use std::path::Path;

use store::Store;

fn user_version(path: &Path) -> i64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn set_user_version(path: &Path, version: i64) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.pragma_update(None, "user_version", version).unwrap();
}

fn versions(store: &Store) -> HashMap<&'static str, i64> {
    store.schema_versions().into_iter().collect()
}

#[test]
fn newer_database_is_rejected_without_touching_any_database() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let store = Store::open(&dir).unwrap();
    let before = versions(&store);
    store.put_setting("guard.sentinel", "keep").unwrap();
    drop(store);

    // catalog 比程序新（必须被拒）；library 故意降到 0——如果预检没拦住，
    // 它会被顺带迁移并升到当前版本，所以这是「没有别的库被改动」的探针。
    set_user_version(&dir.join("catalog.db"), before["catalog"] + 1);
    set_user_version(&dir.join("library.db"), 0);

    let error = match Store::open(&dir) {
        Ok(_) => panic!("newer schema must be rejected"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(
        message.contains("catalog"),
        "error should name the newer database: {message}"
    );
    assert!(
        message.contains(&(before["catalog"] + 1).to_string()),
        "error should report the found version: {message}"
    );

    assert_eq!(user_version(&dir.join("app.db")), before["app"]);
    assert_eq!(user_version(&dir.join("catalog.db")), before["catalog"] + 1);
    assert_eq!(
        user_version(&dir.join("library.db")),
        0,
        "a lower-version database must not be migrated when another is newer"
    );
    assert_eq!(user_version(&dir.join("subscribe.db")), before["subscribe"]);

    let conn = rusqlite::Connection::open(dir.join("app.db")).unwrap();
    let sentinel: String = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'guard.sentinel'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sentinel, "keep", "existing data must be untouched");
}

#[test]
fn fresh_open_and_lower_version_upgrade_still_work() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let store = Store::open(&dir).unwrap();
    let expected = versions(&store);
    drop(store);

    // 幂等重开：版本不变。
    let store = Store::open(&dir).unwrap();
    assert_eq!(versions(&store), expected);
    drop(store);

    // 较低版本 → 正常升级到当前常量。
    set_user_version(&dir.join("library.db"), 0);
    let store = Store::open(&dir).unwrap();
    let upgraded = versions(&store);
    assert_eq!(upgraded["library"], expected["library"]);
    assert!(upgraded["library"] > 0);
    assert_eq!(upgraded["app"], expected["app"]);
    assert_eq!(upgraded["catalog"], expected["catalog"]);
    assert_eq!(upgraded["subscribe"], expected["subscribe"]);
}

#[test]
fn each_newer_database_is_rejected_individually() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("data");
    let store = Store::open(&dir).unwrap();
    let before = versions(&store);
    drop(store);

    for name in ["app", "catalog", "library", "subscribe"] {
        let path = dir.join(format!("{name}.db"));
        let original = user_version(&path);
        set_user_version(&path, before[name] + 1);
        let error = match Store::open(&dir) {
        Ok(_) => panic!("newer schema must be rejected"),
        Err(error) => error,
    };
        assert!(
            error.to_string().contains(name),
            "error for {name} should name it: {error}"
        );
        assert_eq!(user_version(&path), before[name] + 1);
        set_user_version(&path, original);
    }
}
