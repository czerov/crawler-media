//! Store 四个库的**持久**连接属性。
//!
//! `journal_mode=WAL` 写在数据库文件头里，所以外部连接也能验证它确实被设置过。
//! `busy_timeout`/`foreign_keys` 是连接级设置，无法从外部连接反推，由
//! `open.rs` 里的单元测试在**我们自己打开的连接**上断言。

#[test]
fn all_store_databases_use_wal_journal() {
    let tmp = tempfile::tempdir().unwrap();
    drop(store::Store::open(tmp.path()).unwrap());

    for name in ["app.db", "catalog.db", "library.db", "subscribe.db"] {
        let conn = rusqlite::Connection::open(tmp.path().join(name)).unwrap();
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal", "{name} 应为 WAL");
    }
}

/// 重开一个已有的 Store 必须保持幂等，且不改变已设的 WAL。
#[test]
fn reopening_store_keeps_wal() {
    let tmp = tempfile::tempdir().unwrap();
    drop(store::Store::open(tmp.path()).unwrap());
    drop(store::Store::open(tmp.path()).unwrap());

    let conn = rusqlite::Connection::open(tmp.path().join("app.db")).unwrap();
    let mode: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
}
