use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use super::{Store, StoreError, schema};

impl Store {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let data_dir = data_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&data_dir)?;
        Self::open_files(&data_dir, data_dir.join("app.db"))
    }

    pub fn open_at(
        data_dir: impl AsRef<Path>,
        sqlite_path: impl AsRef<Path>,
    ) -> Result<Self, StoreError> {
        Self::open_files(data_dir, sqlite_path)
    }

    fn open_files(
        data_dir: impl AsRef<Path>,
        app_path: impl AsRef<Path>,
    ) -> Result<Self, StoreError> {
        let data_dir = data_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&data_dir)?;
        let app_path = app_path.as_ref().to_path_buf();
        if let Some(parent) = app_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let app = Connection::open(&app_path)?;
        let catalog = Connection::open(data_dir.join("catalog.db"))?;
        let library = Connection::open(data_dir.join("library.db"))?;
        let subscribe = Connection::open(data_dir.join("subscribe.db"))?;
        // 降级保护：在任何迁移 / seed / 连接模式改动之前先做只读预检。
        schema::ensure_supported_versions(&[
            ("app", &app, schema::APP_SCHEMA_VERSION),
            ("catalog", &catalog, schema::CATALOG_SCHEMA_VERSION),
            ("library", &library, schema::LIBRARY_SCHEMA_VERSION),
            ("subscribe", &subscribe, schema::SUBSCRIBE_SCHEMA_VERSION),
        ])?;
        // Enable WAL mode and set busy timeout for all connections.
        for conn in [&app, &catalog, &library, &subscribe] {
            apply_connection_policy(conn)?;
        }
        let store = Self {
            data_dir,
            root_ledger_scans: std::sync::atomic::AtomicUsize::new(0),
            media_ledger_reads: std::sync::atomic::AtomicUsize::new(0),
            role_lookups: std::sync::atomic::AtomicUsize::new(0),
            app_path: app_path.clone(),
            app,
            catalog,
            library,
            subscribe,
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// 媒体库专属数据目录（data/library/covers 等）
    pub fn library_covers_dir(&self) -> PathBuf {
        self.data_dir.join("library").join("covers")
    }

    pub fn sqlite_path(&self) -> &Path {
        &self.app_path
    }

    /// app 库的**真实**路径。`open_at` 允许自定义，因此维护任务不能假定
    /// `data_dir/app.db`（DB8 第 5.2 节的 allowlist 由内部事实产生）。
    pub fn app_db_path(&self) -> &Path {
        &self.app_path
    }

    pub fn db_paths(&self) -> Vec<PathBuf> {
        ["app.db", "catalog.db", "library.db", "subscribe.db"]
            .into_iter()
            .map(|name| self.data_dir.join(name))
            .collect()
    }

    /// Schema versions for each database file (app, catalog, library, subscribe).
    pub fn schema_versions(&self) -> Vec<(&'static str, i64)> {
        [
            ("app", &self.app),
            ("catalog", &self.catalog),
            ("library", &self.library),
            ("subscribe", &self.subscribe),
        ]
        .into_iter()
        .map(|(name, conn)| {
            // 诊断用途的只读访问器：读取失败时报告 0。打开时的降级保护走
            // `schema::ensure_supported_versions`，读取失败会直接返回错误。
            let version = schema::read_schema_version(conn).unwrap_or(0);
            (name, version)
        })
        .collect()
    }

    fn migrate(&self) -> Result<(), StoreError> {
        schema::migrate_app(&self.app)?;
        schema::migrate_catalog(&self.catalog)?;
        schema::migrate_library(&self.library)?;
        super::legacy_recycle::restore_recycle_bin(&self.library)?;
        schema::migrate_subscribe(&self.subscribe)?;
        schema::seed_defaults(&self.app, &self.data_dir)?;
        Ok(())
    }

    pub fn put_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.app.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delete_setting(&self, key: &str) -> Result<bool, StoreError> {
        let changed = self
            .app
            .execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        Ok(changed > 0)
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        self.app
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// 保存一次搜索的结果快照（按 vertical 独立，保留最近 50 条）。
    pub fn insert_search_snapshot(
        &self,
        id: &str,
        vertical: &str,
        query: &str,
        payload: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        self.app.execute(
            "INSERT INTO search_snapshots (id, vertical, query, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, vertical, query, payload, now],
        )?;
        let owner_pattern = id
            .split_once(':')
            .map(|(owner, _)| format!("{owner}:%"))
            .unwrap_or_else(|| id.to_string());
        self.app.execute(
            "DELETE FROM search_snapshots
             WHERE vertical = ?1 AND id LIKE ?2 AND id NOT IN (
                SELECT id FROM search_snapshots WHERE vertical = ?1 AND id LIKE ?2
                ORDER BY created_at DESC LIMIT 50
             )",
            params![vertical, owner_pattern],
        )?;
        Ok(())
    }

    /// `(vertical, query, payload, created_at)`。
    pub fn get_search_snapshot(
        &self,
        id: &str,
    ) -> Result<Option<(String, String, String, i64)>, StoreError> {
        self.app
            .query_row(
                "SELECT vertical, query, payload, created_at FROM search_snapshots WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn delete_search_snapshot(&self, id: &str) -> Result<bool, StoreError> {
        let changed = self
            .app
            .execute("DELETE FROM search_snapshots WHERE id = ?1", params![id])?;
        Ok(changed > 0)
    }

    pub fn clear_search_snapshots(&self) -> Result<(), StoreError> {
        self.app.execute("DELETE FROM search_snapshots", [])?;
        Ok(())
    }
}

/// 所有 Store 连接统一的**连接级**策略。
///
/// `busy_timeout` 与 `foreign_keys` 是连接级设置：bundled SQLite 构建的默认值
/// 恰好是 5000ms / ON，显式设置是为了不依赖构建默认值。`journal_mode=WAL`
/// 是持久属性，写在数据库文件头里。
fn apply_connection_policy(conn: &Connection) -> Result<(), StoreError> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", "5000")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

#[cfg(test)]
mod policy_tests {
    use super::apply_connection_policy;

    /// 连接级 pragma 只能在**我们自己打开的连接**上断言：外部重开连接只会看到
    /// bundled 构建的默认值，证明不了策略被应用。
    #[test]
    fn connection_policy_sets_timeout_and_foreign_keys() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        apply_connection_policy(&conn).unwrap();
        let timeout: i64 = conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .unwrap();
        let foreign_keys: i64 = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5000);
        assert_eq!(foreign_keys, 1);
    }
}
