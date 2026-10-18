//! Recoverable Library removal. Media and user Playback have separate lifetimes.
//!
//! Two storage decisions matter before changing this module:
//!
//! - `library_removal_roots` is the permanent boundary. A removed root keeps
//!   rejecting work from stale paths after the operation succeeds, and the hot
//!   `path_in_removal_scope` check reads only that small table.
//! - `snapshot_json` exists to recover an interrupted removal (ledger identity
//!   and ownership), so it is pruned to `{"rows":[]}` once the operation succeeds.
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{Store, StoreError};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LibraryRemoval {
    pub operation_id: String,
    pub library_id: String,
    pub state: String,
    pub removed_ledger_count: usize,
    pub error: Option<String>,
}

/// Per-file rows of a removal, needed for validation/recovery until it succeeds.
#[derive(Serialize, Deserialize)]
struct Snapshot {
    rows: Vec<RemovalRow>,
}

/// Snapshot layout written before roots moved into `library_removal_roots`.
#[derive(Deserialize)]
struct LegacySnapshot {
    #[serde(default)]
    roots: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct RemovalRow {
    ledger_id: String,
    path: String,
    media_id: String,
    season: Option<u32>,
    episode: Option<u32>,
}

const COLS: &str = "operation_id, library_id, state, removed_ledger_count, error";

/// `settings` keys owned by a Library, stored as `<prefix><library_id>`.
///
/// Library-scoped settings die with the Library; add the prefix here rather
/// than growing a second cleanup path.
const LIBRARY_SETTING_KEY_PREFIXES: &[&str] = &["library_cover_checked."];

fn map_removal(row: &rusqlite::Row<'_>) -> rusqlite::Result<LibraryRemoval> {
    Ok(LibraryRemoval {
        operation_id: row.get(0)?,
        library_id: row.get(1)?,
        state: row.get(2)?,
        removed_ledger_count: row.get(3)?,
        error: row.get(4)?,
    })
}

impl Store {
    /// Deleted roots remain a boundary until explicitly reconfigured as a new Library.
    ///
    /// Reads only the small `library_removal_roots` table, so a path check never
    /// deserializes a snapshot that lists every removed file.
    pub fn path_in_removal_scope(&self, path: &Path) -> Result<bool, StoreError> {
        let path = crate::library_paths::resolved_path(path)?;
        let mut statement = self.app.prepare("SELECT path FROM library_removal_roots")?;
        for root in statement.query_map([], |row| row.get::<_, String>(0))? {
            if path.starts_with(Path::new(&root?)) {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(crate) fn ensure_library_mutable(&self, id: &str) -> Result<(), StoreError> {
        if let Some(removal) = self.removal_for_library(id)? {
            if removal.state != "succeeded" {
                tracing::error!(library_id = id, operation_id = %removal.operation_id,
                    "拒绝修改冻结的 Library");
                return Err(StoreError::Protected(format!("Library {id} 已冻结")));
            }
        }
        Ok(())
    }

    pub(crate) fn ensure_removal_roots_available(&self, paths: &[&str]) -> Result<(), StoreError> {
        let pending = self.pending_library_removals()?;
        if pending.is_empty() {
            return Ok(());
        }
        let proposed = paths
            .iter()
            .filter(|path| !path.trim().is_empty())
            .map(|path| crate::library_paths::resolved_path(Path::new(path.trim())))
            .collect::<Result<Vec<_>, _>>()?;
        for removal in pending {
            for root in self.removal_roots(&removal.operation_id)? {
                let root = crate::library_paths::resolved_path(&root)?;
                if proposed
                    .iter()
                    .any(|path| path.starts_with(&root) || root.starts_with(path))
                {
                    tracing::error!(operation_id = %removal.operation_id, root = %root.display(),
                        "拒绝与待删除 Library 重叠的根目录");
                    return Err(StoreError::Protected("根目录与待删除 Library 重叠".into()));
                }
            }
        }
        Ok(())
    }

    pub fn begin_library_removal(&self, id: &str) -> Result<LibraryRemoval, StoreError> {
        if let Some(existing) = self.removal_for_library(id)? {
            return Ok(existing);
        }
        let library = self
            .get_library(id)?
            .ok_or_else(|| StoreError::Missing(format!("library {id}")))?;
        let mut count = 0;
        for item in self.list_libraries()? {
            if item.kind == library.kind && self.library_accepts_work(&item.id)? {
                count += 1;
            }
        }
        if count <= 1 {
            return Err(StoreError::Protected("每种类型的媒体库至少保留一个".into()));
        }
        let roots = self
            .roots_of(id)?
            .iter()
            .map(|root| crate::library_paths::resolved_path(root))
            .collect::<Result<Vec<_>, _>>()?;
        let mut rows = Vec::new();
        for row in self.list_ledger()? {
            let Some(media) = self.get_media(row.media_id)? else {
                continue;
            };
            if media.kind != library.kind {
                continue;
            }
            let path = Path::new(&row.path);
            let resolved = crate::library_paths::resolved_path(path)?;
            let under_root = roots
                .iter()
                .map(|root| crate::library_paths::resolved_path(root))
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .any(|root| resolved.starts_with(root));
            if !under_root {
                continue;
            }
            match self.library_for_path_strict(path, media.kind)? {
                Some(owner) if owner.id == id => rows.push(RemovalRow {
                    ledger_id: row.id.to_string(),
                    path: row.path,
                    media_id: media.id.to_string(),
                    season: row.season,
                    episode: row.episode,
                }),
                Some(_) => {}
                None => {
                    return Err(StoreError::Protected(
                        "Library 文件归属不唯一，不能安全删除".into(),
                    ));
                }
            }
        }
        let operation_id = uuid::Uuid::new_v4().to_string();
        let tx = self.app.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO library_removals (operation_id, library_id, state, snapshot_json,
             removed_ledger_count, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, 'frozen', ?3, ?4, ?5, ?5)",
            params![
                operation_id,
                id,
                serde_json::to_string(&Snapshot { rows })?,
                0,
                crate::unix_now() * 1000
            ],
        )?;
        for root in &roots {
            tx.execute(
                "INSERT OR IGNORE INTO library_removal_roots (operation_id, path) VALUES (?1, ?2)",
                params![operation_id, root.to_string_lossy()],
            )?;
        }
        tx.commit()?;
        tracing::info!(library_id = id, %operation_id, "Library 删除已冻结");
        self.library_removal(&operation_id)?
            .ok_or_else(|| StoreError::Missing(operation_id))
    }

    pub fn removal_for_library(&self, id: &str) -> Result<Option<LibraryRemoval>, StoreError> {
        Ok(self
            .app
            .query_row(
                &format!("SELECT {COLS} FROM library_removals WHERE library_id=?1"),
                [id],
                map_removal,
            )
            .optional()?)
    }

    pub fn library_removal(&self, id: &str) -> Result<Option<LibraryRemoval>, StoreError> {
        Ok(self
            .app
            .query_row(
                &format!("SELECT {COLS} FROM library_removals WHERE operation_id=?1"),
                [id],
                map_removal,
            )
            .optional()?)
    }

    pub fn pending_library_removals(&self) -> Result<Vec<LibraryRemoval>, StoreError> {
        Ok(self
            .app
            .prepare(&format!(
                "SELECT {COLS} FROM library_removals WHERE state != 'succeeded'"
            ))?
            .query_map([], map_removal)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Boundary roots of one removal operation.
    pub fn removal_roots(&self, id: &str) -> Result<Vec<PathBuf>, StoreError> {
        let mut statement = self.app.prepare(
            "SELECT path FROM library_removal_roots WHERE operation_id = ?1 ORDER BY path",
        )?;
        Ok(statement
            .query_map([id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(PathBuf::from)
            .collect())
    }

    /// Removals that already succeeded, so on-disk artifacts a crash left behind
    /// (for example Library cover files) can be reconciled on startup.
    pub fn succeeded_library_removals(&self) -> Result<Vec<LibraryRemoval>, StoreError> {
        Ok(self
            .app
            .prepare(&format!(
                "SELECT {COLS} FROM library_removals WHERE state = 'succeeded'"
            ))?
            .query_map([], map_removal)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn library_accepts_work(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self.get_library(id)?.is_some() && self.removal_for_library(id)?.is_none())
    }

    fn removal_snapshot(&self, id: &str) -> Result<Snapshot, StoreError> {
        let json: String = self.app.query_row(
            "SELECT snapshot_json FROM library_removals WHERE operation_id=?1",
            [id],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&json)?)
    }

    pub fn removal_ledger_ids(&self, id: &str) -> Result<Vec<String>, StoreError> {
        Ok(self
            .removal_snapshot(id)?
            .rows
            .into_iter()
            .map(|row| row.ledger_id)
            .collect())
    }

    pub fn set_removal_state(
        &self,
        id: &str,
        state: &str,
        error: Option<&str>,
    ) -> Result<(), StoreError> {
        if !matches!(state, "frozen" | "draining" | "cleaning" | "failed") {
            return Err(StoreError::Protected(format!(
                "invalid removal state {state}"
            )));
        }
        let changed = self.app.execute(
            "UPDATE library_removals SET state=?2, error=?3, updated_at_ms=?4
             WHERE operation_id=?1 AND state != 'succeeded'",
            params![id, state, error, crate::unix_now() * 1000],
        )?;
        if changed == 0 {
            return Err(StoreError::Missing(format!("active removal {id}")));
        }
        Ok(())
    }

    pub fn clean_library_removal(&self, id: &str) -> Result<LibraryRemoval, StoreError> {
        let removal = self
            .library_removal(id)?
            .ok_or_else(|| StoreError::Missing(id.into()))?;
        if removal.state == "succeeded" {
            return Ok(removal);
        }
        if removal.state != "cleaning" {
            return Err(StoreError::Protected("Library 工作尚未排空".into()));
        }
        let snapshot = self.removal_snapshot(id)?;
        self.validate_removal_rows(&snapshot, &removal.library_id)?;
        self.clean_removal_rows(&snapshot)?;
        #[cfg(test)]
        removal_cleanup_failpoint()?;
        let tx = self.app.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM library_roots WHERE library_id=?1",
            [&removal.library_id],
        )?;
        tx.execute("DELETE FROM libraries WHERE id=?1", [&removal.library_id])?;
        tx.execute(
            "UPDATE libraries SET is_default=1 WHERE id IN (
            SELECT l.id FROM libraries l WHERE NOT EXISTS (
                SELECT 1 FROM libraries d WHERE d.kind=l.kind AND d.is_default=1)
            AND NOT EXISTS (SELECT 1 FROM library_removals r
                WHERE r.library_id=l.id AND r.state != 'succeeded')
            AND l.id=(SELECT c.id FROM libraries c WHERE c.kind=l.kind
                AND NOT EXISTS (SELECT 1 FROM library_removals r
                    WHERE r.library_id=c.id AND r.state != 'succeeded')
                ORDER BY c.sort_order, c.id LIMIT 1))",
            [],
        )?;
        // Library-scoped settings die with the Library.
        delete_library_scoped_settings(&tx, &removal.library_id)?;
        // The per-file snapshot only exists to recover an interrupted removal;
        // drop it now that this operation is terminal. The boundary roots stay.
        tx.execute(
            "UPDATE library_removals SET state='succeeded', error=NULL,
            removed_ledger_count=?2, snapshot_json=?3, updated_at_ms=?4 WHERE operation_id=?1",
            params![
                id,
                snapshot.rows.len(),
                serde_json::to_string(&Snapshot { rows: Vec::new() })?,
                crate::unix_now() * 1000
            ],
        )?;
        tx.commit()?;
        tracing::info!(operation_id=id, library_id=%removal.library_id,
            removed_ledger_count=snapshot.rows.len(), "Library 索引删除完成，磁盘文件保留");
        self.library_removal(id)?
            .ok_or_else(|| StoreError::Missing(id.into()))
    }

    fn validate_removal_rows(
        &self,
        snapshot: &Snapshot,
        library_id: &str,
    ) -> Result<(), StoreError> {
        for row in &snapshot.rows {
            let current = self
                .library
                .query_row(
                    "SELECT path, media_id, season, episode FROM ledger WHERE id=?1",
                    [&row.ledger_id],
                    |record| {
                        Ok((
                            record.get::<_, String>(0)?,
                            record.get::<_, String>(1)?,
                            record.get::<_, Option<u32>>(2)?,
                            record.get::<_, Option<u32>>(3)?,
                        ))
                    },
                )
                .optional()?;
            // Absent rows are expected after an interrupted library.db commit.
            let Some(current) = current else { continue };
            let unchanged = current
                == (
                    row.path.clone(),
                    row.media_id.clone(),
                    row.season,
                    row.episode,
                );
            let media_id = row
                .media_id
                .parse()
                .map_err(|_| StoreError::Protected("删除快照 Media 身份无效".into()))?;
            let owned = match self.get_media(media_id)? {
                Some(media) => self
                    .library_for_path_strict(Path::new(&row.path), media.kind)?
                    .is_some_and(|owner| owner.id == library_id),
                None => false,
            };
            if !unchanged || !owned {
                tracing::error!(%library_id, ledger_id = %row.ledger_id, "删除快照台账身份或归属已改变");
                return Err(StoreError::Protected("删除快照台账身份或归属已改变".into()));
            }
        }
        Ok(())
    }

    fn clean_removal_rows(&self, snapshot: &Snapshot) -> Result<(), StoreError> {
        let tx = self.library.unchecked_transaction()?;
        for row in &snapshot.rows {
            crate::ledger_cleanup::delete_ledger_by_id_str_in_tx(&tx, &row.ledger_id)?;
        }
        for row in &snapshot.rows {
            // Locked markers are user intent and outlive the files, exactly like
            // `fs_watcher` keeps them when an STRM changes. Only unlocked markers
            // (derived probe output) are cleared, and only once no ledger row for
            // that media/season/episode remains anywhere.
            tx.execute(
                "DELETE FROM media_markers WHERE media_id=?1 AND season IS ?2 AND episode IS ?3
                 AND locked=0
                 AND NOT EXISTS (SELECT 1 FROM ledger WHERE media_id=?1 AND season IS ?2 AND episode IS ?3)",
                params![row.media_id, row.season, row.episode],
            )?;
            if let Some(season) = row.season {
                tx.execute(
                    "DELETE FROM probe_season_comparison_history WHERE media_id=?1 AND season=?2
                     AND NOT EXISTS (SELECT 1 FROM ledger WHERE media_id=?1 AND season=?2)",
                    params![row.media_id, season],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Close active playback sessions matching files being removed, preserving last progress.
    /// Sessions for the same Media hosted in another active Library are unaffected.
    pub fn close_playback_sessions_for_removal(&self, id: &str) -> Result<usize, StoreError> {
        let snapshot = self.removal_snapshot(id)?;
        if snapshot.rows.is_empty() {
            return Ok(0);
        }
        let removal_ledger_ids: std::collections::HashSet<&str> =
            snapshot.rows.iter().map(|r| r.ledger_id.as_str()).collect();

        let mut session_stmt = self.subscribe.prepare(
            "SELECT user_id, device_id, media_id, season, episode, ledger_id FROM playback_sessions",
        )?;
        let sessions = session_stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i32>>(3)?,
                    row.get::<_, Option<i32>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut closed = 0;
        let now = crate::unix_now();
        for (user_id_str, device_id, media_id, season, episode, session_ledger_id) in sessions {
            let matches_removal = if let Some(ref lid) = session_ledger_id {
                removal_ledger_ids.contains(lid.as_str())
            } else {
                let in_snapshot = snapshot.rows.iter().any(|r| {
                    r.media_id == media_id
                        && r.season.map(|s| s as i32) == season
                        && r.episode.map(|e| e as i32) == episode
                });
                if !in_snapshot {
                    false
                } else {
                    let mut ledger_stmt = self.library.prepare(
                        "SELECT id FROM ledger WHERE media_id=?1 AND season IS ?2 AND episode IS ?3",
                    )?;
                    let mut rows_cursor = ledger_stmt.query(params![media_id, season, episode])?;
                    let mut other_exists = false;
                    while let Some(row) = rows_cursor.next()? {
                        let row_id: String = row.get(0)?;
                        if !removal_ledger_ids.contains(row_id.as_str()) {
                            other_exists = true;
                            break;
                        }
                    }
                    !other_exists
                }
            };

            if !matches_removal {
                continue;
            }

            let user_id = user_id_str
                .parse::<domain::UserId>()
                .map_err(|e| StoreError::Protected(format!("会话用户 ID 无效: {e}")))?;
            if self.close_session(user_id, &device_id, now)?.is_some() {
                tracing::info!(%device_id, %media_id, "已关闭待删除媒体库相关的活跃播放会话");
                closed += 1;
            }
        }
        Ok(closed)
    }
}

/// Delete every `settings` row owned by `library_id`, per the prefix registry.
fn delete_library_scoped_settings(
    conn: &Connection,
    library_id: &str,
) -> Result<usize, StoreError> {
    let mut deleted = 0;
    for prefix in LIBRARY_SETTING_KEY_PREFIXES {
        deleted += conn.execute(
            "DELETE FROM settings WHERE key = ?1",
            [format!("{prefix}{library_id}")],
        )?;
    }
    Ok(deleted)
}

/// Drop the settings of Libraries whose removal already succeeded.
///
/// Databases written before removal deleted Library-scoped settings keep
/// `library_cover_checked.{id}` forever, and the setting is only read through
/// `get_library`, so nothing else would ever drop it. Runs on every open rather
/// than behind the schema-version gate: the version was never bumped for this
/// repair, so a database already at the current version would otherwise never
/// run it. Idempotent by construction — it only deletes keys whose Library row
/// is already gone.
///
/// A pending removal keeps its Library row, so its settings are left alone.
pub(crate) fn repair_removed_library_settings(conn: &Connection) -> Result<usize, StoreError> {
    let removed: Vec<String> = conn
        .prepare(
            "SELECT library_id FROM library_removals r
             WHERE r.state = 'succeeded'
               AND NOT EXISTS (SELECT 1 FROM libraries l WHERE l.id = r.library_id)",
        )?
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut deleted = 0;
    for library_id in removed {
        deleted += delete_library_scoped_settings(conn, &library_id)?;
    }
    if deleted > 0 {
        tracing::info!(deleted, "已清理已删除媒体库的专属设置");
    }
    Ok(deleted)
}

/// Backfill `library_removal_roots` from snapshots written before the roots table
/// existed. Idempotent; runs from the app schema migration.
pub(crate) fn backfill_library_removal_roots(conn: &Connection) -> Result<(), StoreError> {
    let snapshots: Vec<(String, String)> = conn
        .prepare("SELECT operation_id, snapshot_json FROM library_removals")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    if snapshots.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    let mut backfilled = 0usize;
    for (operation_id, json) in snapshots {
        let legacy: LegacySnapshot = serde_json::from_str(&json)?;
        for root in legacy.roots {
            backfilled += tx.execute(
                "INSERT OR IGNORE INTO library_removal_roots (operation_id, path) VALUES (?1, ?2)",
                params![operation_id, root.to_string_lossy()],
            )?;
        }
    }
    tx.commit()?;
    if backfilled > 0 {
        tracing::info!(backfilled, "已回填 Library 删除边界根目录");
    }
    Ok(())
}

// Thread-local, unit-test-only injection: never exposed through production configuration.
#[cfg(test)]
std::thread_local! {
    static INTERRUPT_AFTER_LIBRARY_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn removal_cleanup_failpoint() -> Result<(), StoreError> {
    if INTERRUPT_AFTER_LIBRARY_COMMIT.with(|flag| flag.replace(false)) {
        return Err(StoreError::Protected(
            "simulated cleanup interruption".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};

    fn fixture(store: &Store, root: &Path) -> (crate::Library, LedgerRow) {
        let library = store
            .create_library(
                MediaKind::Tv,
                "A",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Example".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        let row = LedgerRow {
            id: LedgerId::new(),
            media_id: media.id,
            path: root.join("E01.strm").display().to_string(),
            season: Some(1),
            episode: Some(1),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        };
        store.insert_ledger(&row).unwrap();
        (library, row)
    }

    #[test]
    fn interruption_between_databases_recovers_original_operation_and_count() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let store = Store::open(&data).unwrap();
        let (library, row) = fixture(&store, &temp.path().join("tv"));
        let removal = store.begin_library_removal(&library.id).unwrap();
        store
            .set_removal_state(&removal.operation_id, "cleaning", None)
            .unwrap();
        INTERRUPT_AFTER_LIBRARY_COMMIT.with(|flag| flag.set(true));
        assert!(store.clean_library_removal(&removal.operation_id).is_err());
        assert!(store.ledger_by_path(&row.path).unwrap().is_none());
        assert!(store.get_library(&library.id).unwrap().is_some());
        assert!(!store.library_accepts_work(&library.id).unwrap());
        drop(store);
        let store = Store::open(&data).unwrap();
        assert_eq!(
            store
                .begin_library_removal(&library.id)
                .unwrap()
                .operation_id,
            removal.operation_id
        );
        let done = store.clean_library_removal(&removal.operation_id).unwrap();
        assert_eq!(done.state, "succeeded");
        assert_eq!(done.removed_ledger_count, 1);
        assert_eq!(
            store.clean_library_removal(&removal.operation_id).unwrap(),
            done
        );
        assert!(store.get_media(row.media_id).unwrap().is_some());
    }

    #[test]
    fn changed_snapshot_identity_rejects_cleanup_before_any_row_is_removed() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("data")).unwrap();
        let (library, row) = fixture(&store, &temp.path().join("tv"));
        let removal = store.begin_library_removal(&library.id).unwrap();
        store
            .set_removal_state(&removal.operation_id, "cleaning", None)
            .unwrap();
        // Simulate a stale writer bypassing the Store freeze barrier.
        store
            .library
            .execute(
                "UPDATE ledger SET episode=2 WHERE id=?1",
                [row.id.to_string()],
            )
            .unwrap();
        assert!(matches!(
            store.clean_library_removal(&removal.operation_id),
            Err(StoreError::Protected(_))
        ));
        assert!(store.ledger_by_path(&row.path).unwrap().is_some());
        assert!(store.get_library(&library.id).unwrap().is_some());
    }

    #[test]
    fn opening_a_pre_fix_database_backfills_removal_boundary_roots() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let store = Store::open(&data).unwrap();
        let (library, _row) = fixture(&store, &temp.path().join("tv"));
        let removal = store.begin_library_removal(&library.id).unwrap();
        let roots = store.removal_roots(&removal.operation_id).unwrap();
        assert_eq!(roots.len(), 1);
        // Rewrite the row into the pre-migration layout: roots live only inside
        // snapshot_json and library_removal_roots is empty.
        let legacy = serde_json::json!({
            "roots": roots.iter().map(|r| r.to_string_lossy()).collect::<Vec<_>>(),
            "rows": [],
        });
        store
            .app
            .execute(
                "UPDATE library_removals SET snapshot_json=?1 WHERE operation_id=?2",
                params![legacy.to_string(), &removal.operation_id],
            )
            .unwrap();
        store
            .app
            .execute("DELETE FROM library_removal_roots", [])
            .unwrap();
        store.app.pragma_update(None, "user_version", 6).unwrap();
        crate::schema::migrate_app(&store.app).unwrap();
        assert!(store
            .path_in_removal_scope(&roots[0].join("E01.strm"))
            .unwrap());
        assert_eq!(
            store.removal_roots(&removal.operation_id).unwrap(),
            roots
        );
    }

    #[test]
    fn opening_a_pre_fix_database_cleans_settings_of_removed_libraries() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let store = Store::open(&data).unwrap();
        let (library, _row) = fixture(&store, &temp.path().join("tv"));
        let kept = store
            .list_libraries()
            .unwrap()
            .iter()
            .find(|l| l.kind == MediaKind::Tv && l.id != library.id)
            .unwrap()
            .id
            .clone();
        let removed_key = format!("library_cover_checked.{}", library.id);
        let kept_key = format!("library_cover_checked.{kept}");
        let removal = store.begin_library_removal(&library.id).unwrap();
        store
            .set_removal_state(&removal.operation_id, "cleaning", None)
            .unwrap();
        store.clean_library_removal(&removal.operation_id).unwrap();
        // Pre-fix residue: the removal succeeded but the Library's setting stayed.
        store.put_setting(&removed_key, "checked-v2").unwrap();
        store.put_setting(&kept_key, "checked-v2").unwrap();
        store.app.pragma_update(None, "user_version", 6).unwrap();
        crate::schema::migrate_app(&store.app).unwrap();

        assert!(store.get_setting(&removed_key).unwrap().is_none());
        assert_eq!(
            store.get_setting(&kept_key).unwrap().as_deref(),
            Some("checked-v2")
        );
    }
}
