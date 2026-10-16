use rusqlite::params;

use super::{Store, StoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogCacheRow {
    pub source: String,
    pub cache_key: String,
    pub body: String,
    pub fetched_at: i64,
    pub expires_at: Option<i64>,
}

impl Store {
    pub fn put_catalog_cache(
        &self,
        source: &str,
        cache_key: &str,
        body: &str,
        fetched_at: i64,
        expires_at: Option<i64>,
    ) -> Result<(), StoreError> {
        self.catalog.execute(
            "INSERT OR REPLACE INTO catalog_cache (source, cache_key, body, fetched_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![source, cache_key, body, fetched_at, expires_at],
        )?;
        Ok(())
    }

    pub fn get_catalog_cache(
        &self,
        source: &str,
        cache_key: &str,
    ) -> Result<Option<String>, StoreError> {
        let mut stmt = self
            .catalog
            .prepare("SELECT body FROM catalog_cache WHERE source = ?1 AND cache_key = ?2")?;
        let mut rows = stmt.query(params![source, cache_key])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    /// All catalog cache rows, newest first.
    ///
    /// `rowid ASC` is the stable tie-break for equal `fetched_at` (e.g. two
    /// languages of one detail fetched in the same second): the first-inserted
    /// row wins, which is also what the targeted
    /// [`Store::catalog_details_for_prefixes`] read returns. The two paths must
    /// agree because the browse fallback and the projection builder both keep
    /// the first row per cache-key base.
    pub fn list_catalog_cache(&self) -> Result<Vec<CatalogCacheRow>, StoreError> {
        let mut stmt = self.catalog.prepare(
            "SELECT source, cache_key, body, fetched_at, expires_at FROM catalog_cache
             ORDER BY fetched_at DESC, rowid ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(CatalogCacheRow {
                source: row.get(0)?,
                cache_key: row.get(1)?,
                body: row.get(2)?,
                fetched_at: row.get(3)?,
                expires_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn delete_catalog_cache(&self, source: &str, cache_key: &str) -> Result<bool, StoreError> {
        let n = self.catalog.execute(
            "DELETE FROM catalog_cache WHERE source = ?1 AND cache_key = ?2",
            params![source, cache_key],
        )?;
        Ok(n > 0)
    }
}
