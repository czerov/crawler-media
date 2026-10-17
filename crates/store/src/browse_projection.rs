//! Browse projection models and methods.

use std::str::FromStr;

use domain::{LedgerId, MediaId};
use rusqlite::params;

use super::browse_changes::ProjectionHead;
use super::browse_pages::BrowseMediaCandidate;
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
    /// `facts_json` 里的 ledger 路径的同源副本：依赖覆盖按列读，不解码 JSON。
    pub path: String,
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
    pub expected_dirty_token: Option<i64>,
    pub files: Vec<BrowseFileFacts>,
    pub media: Vec<BrowseMediaFacts>,
    /// 每 Media 的轻候选：与 `media` 同源写入（`browse_media` 的排序键列 +
    /// `candidate_json`）。缺一条时写空候选并 `warn!`：常用排序的 SQL 分页仍可用，
    /// 高级查询会回退到全量读取。
    pub candidates: Vec<BrowseMediaCandidate>,
}

/// `browse_media` 现有行的发布比对快照（含排序键列与候选 JSON）。
struct ExistingMedia {
    title: String,
    year: Option<u16>,
    facts: String,
    added_at_ns: Option<i64>,
    release_date: String,
    candidate: String,
}

/// Scope 身份指纹：kind + 排序后的根路径。发布与新鲜度检查都必须用这一份实现。
pub fn library_scope_signature(library: &crate::Library) -> String {
    let mut roots: Vec<String> = library
        .root_paths
        .iter()
        .map(|root| root.display().to_string())
        .collect();
    roots.sort_unstable();
    format!("{}|{}", library.kind.as_str(), roots.join("\u{1f}"))
}

impl Store {
    /// 只读 head：`ready` 且 scope 与当前 Library 配置一致的投影代。
    ///
    /// 只读 `browse_generations` 与当前配置，**不装载**任何投影行；热页据此判定
    /// 能否按页读 facts。调用方必须在同一个锁内用它做「批准」与「复核」。
    pub fn fresh_head_locked(&self, library_id: &str) -> Result<Option<ProjectionHead>, StoreError> {
        let scope_signature = self
            .get_library(library_id)?
            .map(|library| library_scope_signature(&library));
        let head = self.browse_projection_head(library_id)?;
        Ok(match head {
            Some(head)
                if head.ready
                    && Some(head.scope_signature.as_str()) == scope_signature.as_deref() =>
            {
                Some(head)
            }
            _ => None,
        })
    }

    /// 在**同一个**锁内复核「被批准的那一代」是否仍然可用：head 仍 ready、scope
    /// 与当前配置一致、`generation` 与 `dirty_token` 都未变。
    ///
    /// 热页的批准（只读 head）与按页读取是两次加锁，中间可能有别的请求消费了
    /// 影响本库的变更（把 head 标脏或换代）。按页读取必须在这里复核，变了就回退，
    /// 否则会用仍然留在表里的旧 facts 出页。
    pub fn browse_page_head_is_current(
        &self,
        library_id: &str,
        approved: &ProjectionHead,
    ) -> Result<bool, StoreError> {
        let Some(current) = self.fresh_head_locked(library_id)? else {
            return Ok(false);
        };
        Ok(current.generation == approved.generation
            && current.dirty_token == approved.dirty_token
            && current.scope_signature == approved.scope_signature)
    }
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
    /// Verifies that library.db dirty token and scope have not changed during build.
    pub fn publish_browse_projection(&self, build: &ProjectionBuild) -> Result<bool, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;

        // CAS check: verify dirty_token in library.db
        let current_head: Option<(i64, String)> = tx
            .query_row(
                "SELECT dirty_token, scope_signature FROM browse_generations WHERE library_id = ?1",
                [&build.library_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        if let Some((cur_dirty_token, _)) = current_head {
            if let Some(expected_dirty) = build.expected_dirty_token {
                if cur_dirty_token != expected_dirty {
                    return Ok(false);
                }
            }
        }

        // Revalidate against the current authoritative library scope in app.db (D3-05)
        if let Ok(Some(current_lib)) = self.get_library(&build.library_id) {
            if library_scope_signature(&current_lib) != build.scope_signature {
                return Ok(false);
            }
        }

        let cur_revs = self.read_revisions()?;
        if !build.revisions.public_matches(&cur_revs) {
            return Ok(false);
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        // Read existing files and media to patch only changed rows
        let mut existing_files: std::collections::HashMap<String, (String, String)> =
            std::collections::HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT ledger_id, path, facts_json FROM browse_files WHERE library_id = ?1",
            )?;
            let rows = stmt.query_map([&build.library_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    (r.get::<_, String>(1)?, r.get::<_, String>(2)?),
                ))
            })?;
            for r in rows {
                let (lid, data) = r?;
                existing_files.insert(lid, data);
            }
        }

        let mut new_file_keys = std::collections::HashSet::new();
        for f in &build.files {
            let lid_str = f.ledger_id.to_string();
            let facts_str = f.facts.to_string();
            new_file_keys.insert(lid_str.clone());

            match existing_files.get(&lid_str) {
                Some((existing_path, existing_facts))
                    if existing_path == &f.path && existing_facts == &facts_str => {}
                Some(_) => {
                    tx.execute(
                        "UPDATE browse_files
                         SET media_id = ?1, generation = ?2, path = ?3, facts_json = ?4
                         WHERE library_id = ?5 AND ledger_id = ?6",
                        params![
                            f.media_id.to_string(),
                            build.generation,
                            f.path,
                            facts_str,
                            build.library_id,
                            lid_str
                        ],
                    )?;
                }
                None => {
                    tx.execute(
                        "INSERT INTO browse_files
                            (library_id, ledger_id, media_id, generation, path, facts_json)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            build.library_id,
                            lid_str,
                            f.media_id.to_string(),
                            build.generation,
                            f.path,
                            facts_str
                        ],
                    )?;
                }
            }
        }

        // Delete files that are no longer in build
        for old_lid in existing_files.keys() {
            if !new_file_keys.contains(old_lid) {
                tx.execute(
                    "DELETE FROM browse_files WHERE library_id = ?1 AND ledger_id = ?2",
                    params![build.library_id, old_lid],
                )?;
            }
        }

        // Read existing media
        let mut existing_media: std::collections::HashMap<String, ExistingMedia> =
            std::collections::HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT media_id, title, year, facts_json, added_at_ns, release_date, candidate_json
                 FROM browse_media WHERE library_id = ?1",
            )?;
            let rows = stmt.query_map([&build.library_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    ExistingMedia {
                        title: r.get(1)?,
                        year: r.get(2)?,
                        facts: r.get(3)?,
                        added_at_ns: r.get(4)?,
                        release_date: r.get(5)?,
                        candidate: r.get(6)?,
                    },
                ))
            })?;
            for r in rows {
                let (mid, data) = r?;
                existing_media.insert(mid, data);
            }
        }

        // 轻候选与 media 行同源写入。缺候选时写空串（读路径据此判定投影过旧、回退
        // 全量读取并 warn 留痕），不拒绝发布：常用排序的 SQL 分页不依赖候选。
        let candidates: std::collections::HashMap<domain::MediaId, &BrowseMediaCandidate> =
            build.candidates.iter().map(|c| (c.media_id, c)).collect();
        let mut new_media_keys = std::collections::HashSet::new();
        for m in &build.media {
            let mid_str = m.media_id.to_string();
            let facts_str = m.facts.to_string();
            new_media_keys.insert(mid_str.clone());
            let candidate = candidates.get(&m.media_id).copied();
            if candidate.is_none() {
                tracing::warn!(
                    library_id = %build.library_id,
                    media_id = %m.media_id,
                    "投影构建缺少轻候选：该库的高级浏览查询将回退到全量读取"
                );
            }
            let candidate_str = candidate
                .map(|candidate| candidate.to_json().to_string())
                .unwrap_or_default();
            let added_at_ns = candidate.and_then(|candidate| candidate.added_at_ns);
            let release_date = candidate
                .map(|candidate| candidate.release_date.clone())
                .unwrap_or_default();

            match existing_media.get(&mid_str) {
                Some(existing)
                    if existing.title == m.title
                        && existing.year == m.year
                        && existing.facts == facts_str
                        && existing.added_at_ns == added_at_ns
                        && existing.release_date == release_date
                        && existing.candidate == candidate_str => {}
                Some(_) => {
                    tx.execute(
                        "UPDATE browse_media
                         SET generation = ?1, title = ?2, year = ?3, facts_json = ?4,
                             added_at_ns = ?5, release_date = ?6, candidate_json = ?7
                         WHERE library_id = ?8 AND media_id = ?9",
                        params![
                            build.generation,
                            m.title,
                            m.year,
                            facts_str,
                            added_at_ns,
                            release_date,
                            candidate_str,
                            build.library_id,
                            mid_str
                        ],
                    )?;
                }
                None => {
                    tx.execute(
                        "INSERT INTO browse_media
                            (library_id, media_id, generation, title, year, facts_json,
                             added_at_ns, release_date, candidate_json)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![
                            build.library_id,
                            mid_str,
                            build.generation,
                            m.title,
                            m.year,
                            facts_str,
                            added_at_ns,
                            release_date,
                            candidate_str
                        ],
                    )?;
                }
            }
        }

        // Delete media no longer in build
        for old_mid in existing_media.keys() {
            if !new_media_keys.contains(old_mid) {
                tx.execute(
                    "DELETE FROM browse_media WHERE library_id = ?1 AND media_id = ?2",
                    params![build.library_id, old_mid],
                )?;
            }
        }

        let dirty_token = build.expected_dirty_token.unwrap_or(0);
        // Upsert generation
        tx.execute(
            "INSERT INTO browse_generations
                (library_id, generation, scope_signature, state, refreshed_at_ms,
                 app_revision, library_revision, catalog_revision, subscribe_revision, dirty_token)
             VALUES (?1, ?2, ?3, 'ready', ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(library_id) DO UPDATE SET
                 generation = ?2,
                 scope_signature = ?3,
                 state = 'ready',
                 refreshed_at_ms = ?4,
                 app_revision = ?5,
                 library_revision = ?6,
                 catalog_revision = ?7,
                 subscribe_revision = ?8,
                 dirty_token = ?9",
            params![
                build.library_id,
                build.generation,
                build.scope_signature,
                now_ms,
                cur_revs.app,
                cur_revs.library,
                cur_revs.catalog,
                cur_revs.subscribe,
                dirty_token
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
            "SELECT ledger_id, media_id, path, facts_json FROM browse_files WHERE library_id = ?1",
        )?;
        let files = files_stmt
            .query_map([library_id], |row| {
                let lid: String = row.get(0)?;
                let mid: String = row.get(1)?;
                let path: String = row.get(2)?;
                let facts_str: String = row.get(3)?;
                let ledger_id = LedgerId::from_str(&lid)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?;
                let media_id = MediaId::from_str(&mid)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?;
                let facts = serde_json::from_str(&facts_str).unwrap_or_default();
                Ok(BrowseFileFacts {
                    ledger_id,
                    media_id,
                    path,
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
