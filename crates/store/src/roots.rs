use std::path::PathBuf;
use std::str::FromStr;

use domain::MediaKind;
use rusqlite::params;

use super::{Store, StoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryRoot {
    pub id: String,
    pub kind: MediaKind,
    pub path: PathBuf,
    pub is_default: bool,
    /// Owning library id (null until backfilled / assigned).
    pub library_id: Option<String>,
    /// Position within the owning library; 0 = primary root.
    pub sort_order: i32,
}

impl Store {
    pub fn list_library_roots(&self) -> Result<Vec<LibraryRoot>, StoreError> {
        let mut stmt = self.app.prepare(
            "SELECT id, kind, path, is_default, library_id, sort_order
             FROM library_roots ORDER BY is_default DESC, sort_order, path",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)? != 0,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i32>(5)?,
            ))
        })?;
        let mut roots = Vec::new();
        for row in rows {
            let (id, kind, path, is_default, library_id, sort_order) = row?;
            roots.push(LibraryRoot {
                id,
                kind: MediaKind::from_str(&kind).map_err(StoreError::MediaKind)?,
                path: PathBuf::from(path),
                is_default,
                library_id,
                sort_order,
            });
        }
        Ok(roots)
    }

    /// Insert a root attached to the kind's default library (extra roots added
    /// through the directory surface land on the default library).
    pub fn insert_library_root(
        &self,
        kind: MediaKind,
        path: &str,
    ) -> Result<LibraryRoot, StoreError> {
        let id = uuid::Uuid::new_v4().to_string();
        let library = self
            .default_library(kind)?
            .ok_or_else(|| StoreError::Missing(format!("default {} library", kind.as_str())))?;
        self.ensure_library_mutable(&library.id)?;
        self.ensure_removal_roots_available(&[path])?;
        let sort_order = self.roots_of(&library.id)?.len() as i32;
        self.app.execute(
            "INSERT INTO library_roots (id, kind, path, is_default, library_id, sort_order)
             VALUES (?1, ?2, ?3, 0, ?4, ?5)",
            params![id, kind.as_str(), path, library.id, sort_order],
        )?;
        Ok(LibraryRoot {
            id,
            kind,
            path: PathBuf::from(path),
            is_default: false,
            library_id: Some(library.id),
            sort_order,
        })
    }

    pub fn get_library_root(&self, id: &str) -> Result<Option<LibraryRoot>, StoreError> {
        Ok(self
            .list_library_roots()?
            .into_iter()
            .find(|root| root.id == id))
    }

    pub fn delete_extra_library_root(&self, id: &str) -> Result<bool, StoreError> {
        if let Some(root) = self.get_library_root(id)? {
            if let Some(owner) = root.library_id {
                self.ensure_library_mutable(&owner)?;
            }
        }
        let n = self.app.execute(
            "DELETE FROM library_roots WHERE id = ?1 AND is_default = 0",
            params![id],
        )?;
        Ok(n > 0)
    }
}
