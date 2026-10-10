//! Recoverable Library removal. Media and user Playback have separate lifetimes.
use std::path::{Path, PathBuf};

use rusqlite::{OptionalExtension, params};
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

#[derive(Serialize, Deserialize)]
struct Snapshot {
    roots: Vec<PathBuf>,
    rows: Vec<RemovalRow>,
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

fn map_removal(row: &rusqlite::Row<'_>) -> rusqlite::Result<LibraryRemoval> {
    Ok(LibraryRemoval { operation_id: row.get(0)?, library_id: row.get(1)?,
        state: row.get(2)?, removed_ledger_count: row.get(3)?, error: row.get(4)? })
}

impl Store {
    pub fn begin_library_removal(&self, id: &str) -> Result<LibraryRemoval, StoreError> {
        if let Some(existing) = self.removal_for_library(id)? {
            return Ok(existing);
        }
        let library = self.get_library(id)?.ok_or_else(|| StoreError::Missing(format!("library {id}")))?;
        let mut count = 0;
        for item in self.list_libraries()? {
            if item.kind == library.kind && self.library_accepts_work(&item.id)? {
                count += 1;
            }
        }
        if count <= 1 {
            return Err(StoreError::Protected("每种类型的媒体库至少保留一个".into()));
        }
        let roots = self.roots_of(id)?;
        let mut rows = Vec::new();
        for row in self.list_ledger()? {
            let Some(media) = self.get_media(row.media_id)? else { continue };
            if media.kind != library.kind { continue; }
            let path = Path::new(&row.path);
            let resolved = crate::library_paths::resolved_path(path)?;
            let under_root = roots.iter().map(|root| crate::library_paths::resolved_path(root))
                .collect::<Result<Vec<_>, _>>()?.iter().any(|root| resolved.starts_with(root));
            if !under_root { continue; }
            match self.library_for_path_strict(path, media.kind)? {
                Some(owner) if owner.id == id => rows.push(RemovalRow {
                    ledger_id: row.id.to_string(), path: row.path, media_id: media.id.to_string(),
                    season: row.season, episode: row.episode,
                }),
                Some(_) => {},
                None => return Err(StoreError::Protected("Library 文件归属不唯一，不能安全删除".into())),
            }
        }
        let operation_id = uuid::Uuid::new_v4().to_string();
        self.app.execute(
            "INSERT INTO library_removals (operation_id, library_id, state, snapshot_json,
             removed_ledger_count, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, 'frozen', ?3, ?4, ?5, ?5)",
            params![operation_id, id, serde_json::to_string(&Snapshot { roots, rows })?,
                0, crate::unix_now() * 1000],
        )?;
        tracing::info!(library_id = id, %operation_id, "Library 删除已冻结");
        self.library_removal(&operation_id)?.ok_or_else(|| StoreError::Missing(operation_id))
    }

    pub fn removal_for_library(&self, id: &str) -> Result<Option<LibraryRemoval>, StoreError> {
        Ok(self.app.query_row(&format!("SELECT {COLS} FROM library_removals WHERE library_id=?1"),
            [id], map_removal).optional()?)
    }

    pub fn library_removal(&self, id: &str) -> Result<Option<LibraryRemoval>, StoreError> {
        Ok(self.app.query_row(&format!("SELECT {COLS} FROM library_removals WHERE operation_id=?1"),
            [id], map_removal).optional()?)
    }

    pub fn pending_library_removals(&self) -> Result<Vec<LibraryRemoval>, StoreError> {
        Ok(self.app.prepare(&format!("SELECT {COLS} FROM library_removals WHERE state != 'succeeded'"))?
            .query_map([], map_removal)?.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn library_accepts_work(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self.get_library(id)?.is_some() && self.removal_for_library(id)?.is_none())
    }

    fn removal_snapshot(&self, id: &str) -> Result<Snapshot, StoreError> {
        let json: String = self.app.query_row(
            "SELECT snapshot_json FROM library_removals WHERE operation_id=?1", [id], |row| row.get(0))?;
        Ok(serde_json::from_str(&json)?)
    }

    pub fn removal_ledger_ids(&self, id: &str) -> Result<Vec<String>, StoreError> {
        Ok(self.removal_snapshot(id)?.rows.into_iter().map(|row| row.ledger_id).collect())
    }

    pub fn set_removal_state(&self, id: &str, state: &str, error: Option<&str>) -> Result<(), StoreError> {
        if !matches!(state, "frozen" | "draining" | "cleaning" | "failed") {
            return Err(StoreError::Protected(format!("invalid removal state {state}")));
        }
        let changed = self.app.execute(
            "UPDATE library_removals SET state=?2, error=?3, updated_at_ms=?4
             WHERE operation_id=?1 AND state != 'succeeded'", params![id, state, error, crate::unix_now()*1000])?;
        if changed == 0 { return Err(StoreError::Missing(format!("active removal {id}"))); }
        Ok(())
    }

    pub fn clean_library_removal(&self, id: &str) -> Result<LibraryRemoval, StoreError> {
        let removal = self.library_removal(id)?.ok_or_else(|| StoreError::Missing(id.into()))?;
        if removal.state == "succeeded" { return Ok(removal); }
        if removal.state != "cleaning" {
            return Err(StoreError::Protected("Library 工作尚未排空".into()));
        }
        let snapshot = self.removal_snapshot(id)?;
        self.clean_removal_rows(&snapshot)?;
        let tx = self.app.unchecked_transaction()?;
        tx.execute("DELETE FROM library_roots WHERE library_id=?1", [&removal.library_id])?;
        tx.execute("DELETE FROM libraries WHERE id=?1", [&removal.library_id])?;
        crate::library_defaults::ensure_library_defaults(&tx)?;
        tx.execute("UPDATE library_removals SET state='succeeded', error=NULL,
            removed_ledger_count=?2, updated_at_ms=?3 WHERE operation_id=?1",
            params![id, snapshot.rows.len(), crate::unix_now()*1000])?;
        tx.commit()?;
        tracing::info!(operation_id=id, library_id=%removal.library_id,
            removed_ledger_count=snapshot.rows.len(), "Library 索引删除完成，磁盘文件保留");
        self.library_removal(id)?.ok_or_else(|| StoreError::Missing(id.into()))
    }

    fn clean_removal_rows(&self, snapshot: &Snapshot) -> Result<(), StoreError> {
        let tx = self.library.unchecked_transaction()?;
        for row in &snapshot.rows {
            crate::library::delete_file_meta_for_ledger(&tx, &row.ledger_id)?;
            tx.execute("DELETE FROM ledger WHERE id=?1", [&row.ledger_id])?;
        }
        for row in &snapshot.rows {
            tx.execute("DELETE FROM media_markers WHERE media_id=?1 AND season IS ?2 AND episode IS ?3
                AND NOT EXISTS (SELECT 1 FROM ledger WHERE media_id=?1 AND season IS ?2 AND episode IS ?3)",
                params![row.media_id, row.season, row.episode])?;
            tx.execute("DELETE FROM probe_season_comparison_history WHERE media_id=?1 AND season=?2",
                params![row.media_id, row.season.unwrap_or(1)])?;
        }
        tx.commit()?;
        Ok(())
    }
}
