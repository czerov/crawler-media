//! jobs 队列库的持久连接属性（`journal_mode` 是文件头属性，外部连接可验证）。

#[test]
fn queue_database_uses_wal_journal() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("jobs.db");
    drop(jobs::Queue::open(&path).unwrap());

    let conn = rusqlite::Connection::open(&path).unwrap();
    let mode: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal", "jobs.db 应为 WAL");
}
