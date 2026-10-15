//! Browse projection models and methods.

use std::str::FromStr;

use domain::{LedgerId, MediaId};
use rusqlite::params;

use super::{CatalogCacheRow, Store, StoreError};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadRevisions {
    pub app: i64,
    pub library: i64,
    pub catalog: i64,
    pub subscribe: i64,
}

impl ReadRevisions {
    /// Whether the two revision sets imply the same **public** browse projection.
    ///
    /// The shared image/NFO projection stores only public facts (Media, ledger,
    /// catalog metadata, artwork presence). Per-user watch state is read live
    /// per request, so `subscribe` DML (`playback_units` heartbeats) must not
    /// invalidate or discard a public projection; only user-derived watch
    /// aggregates are invalidated by those writes.
    pub fn public_matches(&self, other: &Self) -> bool {
        self.app == other.app && self.library == other.library && self.catalog == other.catalog
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowseFileFacts {
    pub ledger_id: LedgerId,
    pub media_id: MediaId,
    pub facts: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowseMediaFacts {
    pub media_id: MediaId,
    pub title: String,
    pub year: Option<u16>,
    pub facts: serde_json::Value,
}

#[derive(Clone, Debug)]
pub struct ProjectionBuild {
    pub library_id: String,
    pub generation: i64,
    pub scope_signature: String,
    pub revisions: ReadRevisions,
    pub files: Vec<BrowseFileFacts>,
    pub media: Vec<BrowseMediaFacts>,
}

#[derive(Clone, Debug)]
pub struct ProjectionSnapshot {
    pub library_id: String,
    pub generation: i64,
    pub scope_signature: String,
    pub state: String,
    pub refreshed_at_ms: i64,
    pub revisions: ReadRevisions,
    pub files: Vec<BrowseFileFacts>,
    pub media: Vec<BrowseMediaFacts>,
}

impl Store {
    /// Read the latest revisions from all 4 SQLite databases.
    pub fn read_revisions(&self) -> Result<ReadRevisions, StoreError> {
        let app = self
            .app
            .query_row(
                "SELECT revision FROM _read_revisions WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let library = self
            .library
            .query_row(
                "SELECT revision FROM _read_revisions WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let catalog = self
            .catalog
            .query_row(
                "SELECT revision FROM _read_revisions WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let subscribe = self
            .subscribe
            .query_row(
                "SELECT revision FROM _read_revisions WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);

        Ok(ReadRevisions {
            app,
            library,
            catalog,
            subscribe,
        })
    }

    /// Publish a rebuilt browse projection within a single Immediate transaction in library.db.
    ///
    /// Verifies that library.db revision has not changed during build.
    pub fn publish_browse_projection(&self, build: &ProjectionBuild) -> Result<bool, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;

        // 只核对公共事实 revision：构建期间的 `playback_units` 写入不应丢弃
        // 已经建好的公共图片/NFO 投影（用户 unit 每请求实时批量读取）。
        let cur_revs = self.read_revisions()?;
        if !cur_revs.public_matches(&build.revisions) {
            return Ok(false);
        }

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        // Delete previous projection data for this library
        tx.execute(
            "DELETE FROM browse_files WHERE library_id = ?1",
            [&build.library_id],
        )?;
        tx.execute(
            "DELETE FROM browse_media WHERE library_id = ?1",
            [&build.library_id],
        )?;

        // Insert new files
        for f in &build.files {
            tx.execute(
                "INSERT INTO browse_files (library_id, ledger_id, media_id, generation, facts_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    build.library_id,
                    f.ledger_id.to_string(),
                    f.media_id.to_string(),
                    build.generation,
                    f.facts.to_string()
                ],
            )?;
        }

        // Insert new media
        for m in &build.media {
            tx.execute(
                "INSERT INTO browse_media (library_id, media_id, generation, title, year, facts_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    build.library_id,
                    m.media_id.to_string(),
                    build.generation,
                    m.title,
                    m.year,
                    m.facts.to_string()
                ],
            )?;
        }

        // Upsert generation
        tx.execute(
            "INSERT OR REPLACE INTO browse_generations
                (library_id, generation, scope_signature, state, refreshed_at_ms,
                 app_revision, library_revision, catalog_revision, subscribe_revision)
             VALUES (?1, ?2, ?3, 'ready', ?4, ?5, ?6, ?7, ?8)",
            params![
                build.library_id,
                build.generation,
                build.scope_signature,
                now_ms,
                build.revisions.app,
                build.revisions.library,
                build.revisions.catalog,
                build.revisions.subscribe
            ],
        )?;

        tx.commit()?;
        Ok(true)
    }

    /// Retrieve the current browse projection snapshot for a library.
    pub fn browse_projection(&self, library_id: &str) -> Result<Option<ProjectionSnapshot>, StoreError> {
        let gen_opt: Option<(i64, String, String, i64, i64, i64, i64, i64)> = self
            .library
            .query_row(
                "SELECT generation, scope_signature, state, refreshed_at_ms,
                        app_revision, library_revision, catalog_revision, subscribe_revision
                 FROM browse_generations WHERE library_id = ?1 AND state = 'ready'",
                [library_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )
            .optional()?;

        let Some((generation, scope_signature, state, refreshed_at_ms, app, lib, cat, sub)) = gen_opt
        else {
            return Ok(None);
        };

        let mut files_stmt = self.library.prepare(
            "SELECT ledger_id, media_id, facts_json FROM browse_files WHERE library_id = ?1",
        )?;
        let files = files_stmt
            .query_map([library_id], |row| {
                let lid: String = row.get(0)?;
                let mid: String = row.get(1)?;
                let facts_str: String = row.get(2)?;
                let ledger_id = LedgerId::from_str(&lid)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?;
                let media_id = MediaId::from_str(&mid)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?;
                let facts = serde_json::from_str(&facts_str).unwrap_or_default();
                Ok(BrowseFileFacts {
                    ledger_id,
                    media_id,
                    facts,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut media_stmt = self.library.prepare(
            "SELECT media_id, title, year, facts_json FROM browse_media WHERE library_id = ?1 ORDER BY title ASC",
        )?;
        let media = media_stmt
            .query_map([library_id], |row| {
                let mid: String = row.get(0)?;
                let title: String = row.get(1)?;
                let year: Option<u16> = row.get(2)?;
                let facts_str: String = row.get(3)?;
                let media_id = MediaId::from_str(&mid)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?;
                let facts = serde_json::from_str(&facts_str).unwrap_or_default();
                Ok(BrowseMediaFacts {
                    media_id,
                    title,
                    year,
                    facts,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Some(ProjectionSnapshot {
            library_id: library_id.to_string(),
            generation,
            scope_signature,
            state,
            refreshed_at_ms,
            revisions: ReadRevisions {
                app,
                library: lib,
                catalog: cat,
                subscribe: sub,
            },
            files,
            media,
        }))
    }

    /// Mark browse projection generation as dirty for a library (or all libraries if None).
    pub fn invalidate_browse_projection(&self, library_id: Option<&str>) -> Result<(), StoreError> {
        if let Some(id) = library_id {
            self.library.execute(
                "UPDATE browse_generations SET state = 'dirty' WHERE library_id = ?1",
                [id],
            )?;
        } else {
            self.library.execute(
                "UPDATE browse_generations SET state = 'dirty'",
                [],
            )?;
        }
        Ok(())
    }

    /// Query catalog details for specific (kind, id) tuples in batches of up to 500.
    pub fn catalog_details_batch(
        &self,
        keys: &[(String, String)],
    ) -> Result<Vec<CatalogCacheRow>, StoreError> {
        let mut results = Vec::new();
        for chunk in keys.chunks(500) {
            for (source, cache_key) in chunk {
                let row = self
                    .catalog
                    .query_row(
                        "SELECT source, cache_key, body, fetched_at, expires_at
                         FROM catalog_cache
                         WHERE source = ?1 AND cache_key = ?2",
                        params![source, cache_key],
                        |r| {
                            Ok(CatalogCacheRow {
                                source: r.get(0)?,
                                cache_key: r.get(1)?,
                                body: r.get(2)?,
                                fetched_at: r.get(3)?,
                                expires_at: r.get(4)?,
                            })
                        },
                    )
                    .optional()?;
                if let Some(r) = row {
                    results.push(r);
                }
            }
        }
        Ok(results)
    }

    /// Query the detail rows whose cache key is exactly `prefix` or `prefix?<params>`.
    ///
    /// The browse metadata reader indexes rows by the part before `?`, keeping the
    /// newest per base; this mirrors that lookup without scanning every catalog
    /// body. `ORDER BY fetched_at DESC, rowid ASC` is the **same stable tie-break**
    /// as `list_catalog_cache`, so a tied `fetched_at` (two languages of one
    /// detail) resolves to the same row on both the targeted and the full read.
    pub fn catalog_details_for_prefixes(
        &self,
        source: &str,
        prefixes: &[String],
    ) -> Result<Vec<CatalogCacheRow>, StoreError> {
        self.catalog_details_for_prefixes_with_vm_steps(source, prefixes)
            .map(|(rows, _)| rows)
    }

    /// Same read as [`Self::catalog_details_for_prefixes`], additionally
    /// reporting the SQLite VM steps the query consumed so tests can assert DB work.
    #[doc(hidden)]
    pub fn catalog_details_for_prefixes_with_vm_steps(
        &self,
        source: &str,
        prefixes: &[String],
    ) -> Result<(Vec<CatalogCacheRow>, i64), StoreError> {
        if prefixes.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let mut results = Vec::new();
        let mut total_vm_steps: i64 = 0;
        for chunk in prefixes.chunks(400) {
            let mut sql = String::from(
                "SELECT source, cache_key, body, fetched_at, expires_at
                 FROM catalog_cache WHERE source = ?1 AND (",
            );
            let mut values: Vec<rusqlite::types::Value> =
                vec![rusqlite::types::Value::from(source.to_string())];
            for (index, prefix) in chunk.iter().enumerate() {
                if index > 0 {
                    sql.push_str(" OR ");
                }
                let exact = values.len() + 1;
                let lower = values.len() + 2;
                let upper = values.len() + 3;
                sql.push_str(&format!(
                    "cache_key = ?{exact} OR (cache_key >= ?{lower} AND cache_key < ?{upper})"
                ));
                values.push(rusqlite::types::Value::from(prefix.clone()));
                values.push(rusqlite::types::Value::from(format!("{prefix}?")));
                values.push(rusqlite::types::Value::from(format!("{prefix}@")));
            }
            sql.push_str(") ORDER BY fetched_at DESC, rowid ASC");
            let mut stmt = self.catalog.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(values), |r| {
                Ok(CatalogCacheRow {
                    source: r.get(0)?,
                    cache_key: r.get(1)?,
                    body: r.get(2)?,
                    fetched_at: r.get(3)?,
                    expires_at: r.get(4)?,
                })
            })?;
            for row in rows {
                results.push(row?);
            }
            total_vm_steps += i64::from(stmt.get_status(rusqlite::StatementStatus::VmStep));
        }
        Ok((results, total_vm_steps))
    }
}

trait RusqliteOptionalExt<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> RusqliteOptionalExt<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
