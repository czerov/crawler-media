use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};

use crate::client::TmdbError;

pub const DEFAULT_TTL_SECS: i64 = 7 * 24 * 60 * 60;

/// Real unix seconds for cache expiry. `new_at(..., 0)` stays available for
/// tests; production `new()` must not use 0 or cache never expires.
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

pub struct CatalogCache {
    conn: Mutex<Connection>,
    ttl_secs: i64,
    /// 测试用固定时钟。`None` = 每次访问都取实时时钟（生产路径），
    /// 避免「进程启动时钉死的 now 让缓存永不过期」。
    now: Mutex<Option<i64>>,
    source: &'static str,
}

impl CatalogCache {
    pub fn open(path: impl AsRef<Path>, now: i64) -> Result<Self, TmdbError> {
        Self::open_source(path, now, "tmdb")
    }

    pub fn open_source(
        path: impl AsRef<Path>,
        now: i64,
        source: &'static str,
    ) -> Result<Self, TmdbError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        apply_connection_policy(&conn)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS catalog_cache (
                source TEXT NOT NULL,
                cache_key TEXT NOT NULL,
                body TEXT NOT NULL,
                fetched_at INTEGER NOT NULL,
                expires_at INTEGER,
                PRIMARY KEY (source, cache_key)
            );
            "#,
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            ttl_secs: DEFAULT_TTL_SECS,
            // now == 0 → 实时时钟（生产）；非 0 → 测试钉住的时钟。
            now: Mutex::new((now != 0).then_some(now)),
            source,
        })
    }

    /// 测试用：把时钟钉到指定值（生产不要调用，0 表示实时）。
    pub fn set_now(&self, now: i64) {
        *self.now.lock() = Some(now);
    }

    fn now_secs(&self) -> i64 {
        self.now.lock().unwrap_or_else(crate::cache::unix_now)
    }

    pub fn get_fresh(&self, key: &str) -> Result<Option<String>, TmdbError> {
        self.load(key, false)
    }

    pub fn get_stale(&self, key: &str) -> Result<Option<String>, TmdbError> {
        self.load(key, true)
    }

    pub fn put(&self, key: &str, body: &str) -> Result<(), TmdbError> {
        let now = self.now_secs();
        let expires = now + self.ttl_secs;
        self.conn.lock().execute(
            "INSERT OR REPLACE INTO catalog_cache (source, cache_key, body, fetched_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![self.source, key, body, now, expires],
        )?;
        Ok(())
    }

    /// Decode at the public endpoint's contract before replacing durable bytes.
    pub(crate) fn get_or_fetch<T>(
        &self,
        http: &impl crate::client::CatalogGet,
        key: &str,
        decode: impl Fn(&str) -> Result<T, TmdbError>,
    ) -> Result<T, TmdbError> {
        if let Some(body) = self.get_fresh(key)? {
            match decode(&body) {
                Ok(value) => {
                    tracing::debug!(source = self.source, path = key, "Metadata cache hit");
                    return Ok(value);
                }
                Err(error) => {
                    tracing::warn!(source = self.source, path = key, %error, "Malformed fresh Metadata cache; refetching");
                }
            }
        }
        let fetched = http.get(key).and_then(|body| {
            let value = decode(&body)?;
            self.put(key, &body)?;
            Ok(value)
        });
        match fetched {
            Ok(value) => Ok(value),
            Err(error) => {
                tracing::warn!(source = self.source, path = key, %error, "Metadata refresh failed; trying valid stale cache");
                if let Some(body) = self.get_stale(key)? {
                    match decode(&body) {
                        Ok(value) => {
                            tracing::info!(
                                source = self.source,
                                path = key,
                                "Using valid stale Metadata cache"
                            );
                            return Ok(value);
                        }
                        Err(stale_error) => {
                            tracing::warn!(source = self.source, path = key, %stale_error, "Rejected malformed stale Metadata cache");
                        }
                    }
                }
                tracing::error!(source = self.source, path = key, %error, "Metadata unavailable without valid cache");
                Err(error)
            }
        }
    }

    fn load(&self, key: &str, allow_stale: bool) -> Result<Option<String>, TmdbError> {
        let now = self.now_secs();
        let conn = self.conn.lock();
        let row: Option<(String, Option<i64>)> = conn
            .query_row(
                "SELECT body, expires_at FROM catalog_cache WHERE source = ?1 AND cache_key = ?2",
                params![self.source, key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((body, expires_at)) = row else {
            return Ok(None);
        };
        if !allow_stale {
            if let Some(expires) = expires_at {
                if expires <= now {
                    return Ok(None);
                }
            }
        }
        Ok(Some(body))
    }
}

/// 所有 `CatalogCache` 连接（含 fanart 缓存使用的同一个 `open_source`）统一的
/// **连接级**策略。`busy_timeout`/`foreign_keys` 是连接级设置，bundled 构建默认
/// 5000ms/ON，显式设置是为了不依赖构建默认值；`journal_mode=WAL` 是持久属性。
fn apply_connection_policy(conn: &Connection) -> Result<(), TmdbError> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", "5000")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

#[cfg(test)]
mod policy_tests {
    use super::apply_connection_policy;

    /// 连接级 pragma 必须在**我们自己打开的连接**上断言：外部重开连接只会看到
    /// bundled 构建的默认值，证明不了策略被应用。
    #[test]
    fn catalog_cache_connection_policy_sets_timeout_and_foreign_keys() {
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
