//! 目录缓存库（`catalog.db`，以及复用同一 `open_source` 的 `fanart.db`）的
//! 持久连接属性。fanart 曾经是唯一没有显式 WAL 的库。

use std::path::Path;

struct Offline;

impl media::CatalogGet for Offline {
    fn get(&self, _path: &str) -> Result<String, media::TmdbError> {
        Err(media::TmdbError::Http("offline test".into()))
    }
}

fn assert_wal(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    let mode: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal", "{} 应为 WAL", path.display());
}

#[test]
fn catalog_cache_database_uses_wal_journal() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("catalog.db");
    drop(media::Tmdb::new(Offline, &path).unwrap());
    assert_wal(&path);
}
