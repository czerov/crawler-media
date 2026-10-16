use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::{Store, StoreError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BrowseChangeSource {
    App,
    Library,
    Catalog,
}

impl BrowseChangeSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Library => "library",
            Self::Catalog => "catalog",
        }
    }

    pub fn from_str_source(s: &str) -> Option<Self> {
        match s {
            "app" => Some(Self::App),
            "library" => Some(Self::Library),
            "catalog" => Some(Self::Catalog),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowseChange {
    pub source: BrowseChangeSource,
    pub seq: i64,
    pub entity_kind: String,
    pub entity_key: String,
    pub old_path: Option<String>,
    pub new_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionHead {
    pub generation: i64,
    pub dirty_token: i64,
    pub scope_signature: String,
    pub ready: bool,
    pub refreshed_at_ms: i64,
}

impl Store {
    /// Read changes recorded in `_browse_changes` table for the given source after sequence `after`.
    pub fn browse_changes(
        &self,
        source: BrowseChangeSource,
        after: i64,
        limit: usize,
    ) -> Result<Vec<BrowseChange>, StoreError> {
        let conn = match source {
            BrowseChangeSource::App => &self.app,
            BrowseChangeSource::Library => &self.library,
            BrowseChangeSource::Catalog => &self.catalog,
        };

        let mut stmt = conn.prepare(
            "SELECT seq, entity_kind, entity_key, old_path, new_path
             FROM _browse_changes
             WHERE seq > ?1
             ORDER BY seq ASC
             LIMIT ?2",
        )?;

        let rows = stmt.query_map(params![after, limit as i64], |row| {
            Ok(BrowseChange {
                source,
                seq: row.get(0)?,
                entity_kind: row.get(1)?,
                entity_key: row.get(2)?,
                old_path: row.get(3)?,
                new_path: row.get(4)?,
            })
        })?;

        let mut changes = Vec::new();
        for r in rows {
            changes.push(r?);
        }
        Ok(changes)
    }

    /// Read the committed cursor for a given source in `library.db`.
    pub fn browse_change_cursor(&self, source: BrowseChangeSource) -> Result<i64, StoreError> {
        let seq = self
            .library
            .query_row(
                "SELECT last_seq FROM browse_change_cursors WHERE source = ?1",
                params![source.as_str()],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(seq)
    }

    /// Apply browse changes:
    /// In a SINGLE Immediate transaction on `library.db`:
    /// 1. Increment `dirty_token` (or mark dirty) for each affected library in `browse_generations`.
    /// 2. Update `browse_change_cursors` for `source` to `through`.
    /// Then (after commit), delete confirmed changes (`seq <= through`) from source `_browse_changes`.
    pub fn apply_browse_changes(
        &self,
        source: BrowseChangeSource,
        through: i64,
        affected_libraries: &[String],
    ) -> Result<(), StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;

        // 1. Dirty persisted BEFORE cursor advances.
        // Mark state = 'dirty' atomically with dirty_token increment so freshness
        // immediately stops serving the old projection.
        for lib_id in affected_libraries {
            let updated = tx.execute(
                "UPDATE browse_generations
                 SET dirty_token = dirty_token + 1,
                     state = 'dirty'
                 WHERE library_id = ?1",
                params![lib_id],
            )?;
            if updated == 0 {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                tx.execute(
                    "INSERT OR IGNORE INTO browse_generations
                     (library_id, generation, scope_signature, state, refreshed_at_ms,
                      app_revision, library_revision, catalog_revision, subscribe_revision, dirty_token)
                     VALUES (?1, 0, '', 'dirty', ?2, 0, 0, 0, 0, 1)",
                    params![lib_id, now_ms],
                )?;
            }
        }

        // 2. Cursor advances
        tx.execute(
            "INSERT INTO browse_change_cursors (source, last_seq)
             VALUES (?1, ?2)
             ON CONFLICT(source) DO UPDATE SET last_seq = ?2 WHERE last_seq < ?2",
            params![source.as_str(), through],
        )?;

        tx.commit()?;

        // 3. Source pruning in bounded batches after transaction commits (D3-07).
        let source_conn = match source {
            BrowseChangeSource::App => &self.app,
            BrowseChangeSource::Library => &self.library,
            BrowseChangeSource::Catalog => &self.catalog,
        };
        loop {
            let deleted = source_conn.execute(
                "DELETE FROM _browse_changes WHERE rowid IN (
                     SELECT rowid FROM _browse_changes WHERE seq <= ?1 LIMIT 500
                 )",
                params![through],
            )?;
            if deleted < 500 {
                break;
            }
        }
        Ok(())
    }

    /// Read the lightweight projection head for a library.
    pub fn browse_projection_head(
        &self,
        library_id: &str,
    ) -> Result<Option<ProjectionHead>, StoreError> {
        let row = self
            .library
            .query_row(
                "SELECT generation, dirty_token, scope_signature, state, refreshed_at_ms
                 FROM browse_generations
                 WHERE library_id = ?1",
                params![library_id],
                |r| {
                    let generation: i64 = r.get(0)?;
                    let dirty_token: i64 = r.get(1)?;
                    let scope_signature: String = r.get(2)?;
                    let state: String = r.get(3)?;
                    let refreshed_at_ms: i64 = r.get(4)?;
                    Ok(ProjectionHead {
                        generation,
                        dirty_token,
                        scope_signature,
                        ready: state == "ready",
                        refreshed_at_ms,
                    })
                },
            );

        match row {
            Ok(head) => Ok(Some(head)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StoreError::Sqlite(e)),
        }
    }
}
