//! Scoped scanning publication and ownership verification.
//!
//! Filters walked paths so that only those strictly owned by the requested Library
//! (under the same MediaKind) are admitted, and validates that the Library
//! configuration and state have not changed before publishing.

use std::path::{Path, PathBuf};

use store::{LibraryPathScope, Store, StrictOwner};

use crate::library_presence::PresenceError;
use crate::management::ApiState;

pub struct ScanScope {
    pub library_id: String,
    pub kind: domain::MediaKind,
    scope: LibraryPathScope,
}

impl ScanScope {
    /// Capture and resolve the strict scope for a library outside the Store lock.
    pub fn capture(state: &ApiState, library_id: &str) -> Result<Self, PresenceError> {
        let (snapshot, kind) = {
            let store_arc = state.store();
            let guard = store_arc.lock();
            let snapshot = guard.library_ownership()?;
            let kind = snapshot
                .library(library_id)
                .ok_or(PresenceError::Missing)?
                .kind;
            if !snapshot.accepts_work(library_id) {
                return Err(PresenceError::Deleting);
            }
            (snapshot, kind)
        };

        let scope = LibraryPathScope::resolve(snapshot)?;
        if !scope.accepts_work(library_id) {
            return Err(PresenceError::Deleting);
        }

        Ok(Self {
            library_id: library_id.to_string(),
            kind,
            scope,
        })
    }

    /// Filter paths to retain only those strictly owned by this library.
    pub fn filter_paths(&self, paths: Vec<PathBuf>) -> Result<Vec<PathBuf>, PresenceError> {
        let mut admitted = Vec::with_capacity(paths.len());
        for p in paths {
            let Ok(resolved) = LibraryPathScope::resolve_path(&p) else {
                continue;
            };
            match self.scope.owner(&resolved, self.kind) {
                StrictOwner::Owned(lib) if lib.id == self.library_id => {
                    admitted.push(p);
                }
                StrictOwner::Ambiguous => return Err(PresenceError::Ambiguous),
                _ => {}
            }
        }
        Ok(admitted)
    }

    /// Filter transferred files to retain only those strictly owned by this library.
    pub fn filter_transferred(
        &self,
        files: Vec<library::TransferredFile>,
    ) -> Result<Vec<library::TransferredFile>, PresenceError> {
        let mut admitted = Vec::with_capacity(files.len());
        for file in files {
            let Ok(resolved) = LibraryPathScope::resolve_path(&file.path) else {
                continue;
            };
            match self.scope.owner(&resolved, self.kind) {
                StrictOwner::Owned(lib) if lib.id == self.library_id => {
                    admitted.push(file);
                }
                StrictOwner::Ambiguous => return Err(PresenceError::Ambiguous),
                _ => {}
            }
        }
        Ok(admitted)
    }

    /// Validate in-lock that the library is still active and matches the captured scope.
    pub fn validate(&self, store: &Store) -> Result<(), PresenceError> {
        let current_snapshot = store.library_ownership()?;
        if !current_snapshot.accepts_work(&self.library_id) {
            return Err(PresenceError::Deleting);
        }
        if !self.scope.matches(&current_snapshot) {
            return Err(PresenceError::ScopeChanged);
        }
        Ok(())
    }
}
