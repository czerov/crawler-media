//! Validated strict Library path scope for safe filesystem observations
//! and ownership boundary decisions.
//!
//! Unlike [`super::library_ownership::LibraryOwnership`], this scope:
//! 1. Resolves and normalizes all root paths out of the Store lock.
//! 2. Does NOT fall back to kind defaults when a path does not match any root.
//! 3. Strictly handles ambiguity and unmatched outcomes.
//! 4. Preserves original Library configurations and detects stale snapshots.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use domain::MediaKind;

use crate::library_ownership::LibraryOwnership;
use crate::library_paths::resolved_path;
use crate::{Library, StoreError};

/// The outcome of matching a path strictly against library roots.
#[derive(Debug, PartialEq, Eq)]
pub enum StrictOwner<'a> {
    Owned(&'a Library),
    Unmatched,
    Ambiguous,
}

#[derive(Clone, Debug)]
struct ResolvedLibrary {
    library: Library,
    resolved_roots: Vec<PathBuf>,
}

/// A frozen, normalized scope of library roots and active states.
#[derive(Clone, Debug)]
pub struct LibraryPathScope {
    libraries: Vec<ResolvedLibrary>,
    blocked: HashSet<String>,
}

impl LibraryPathScope {
    /// Resolve and normalize all root paths in the given snapshot.
    ///
    /// MUST be called outside Store locks because filesystem canonicalization
    /// is performed for existing root paths.
    pub fn resolve(snapshot: LibraryOwnership) -> Result<Self, StoreError> {
        let mut libraries = Vec::with_capacity(snapshot.libraries().len());
        for lib in snapshot.libraries() {
            let mut resolved_roots = Vec::with_capacity(lib.root_paths.len());
            for root in &lib.root_paths {
                let r = resolved_path(root)?;
                resolved_roots.push(r);
            }
            libraries.push(ResolvedLibrary {
                library: lib.clone(),
                resolved_roots,
            });
        }
        let blocked = snapshot
            .libraries()
            .iter()
            .filter(|lib| !snapshot.accepts_work(&lib.id))
            .map(|lib| lib.id.clone())
            .collect();
        Ok(Self { libraries, blocked })
    }

    /// Resolve a target path using the standard filesystem resolver.
    pub fn resolve_path(path: &Path) -> Result<PathBuf, StoreError> {
        Ok(resolved_path(path)?)
    }

    /// Strict ownership test: only matching roots within the same MediaKind.
    ///
    /// Largest matching root component count wins.
    /// Equal depth reached by two different libraries is ambiguous.
    /// Miss returns Unmatched (NEVER falls back to default).
    pub fn owner(&self, resolved_path: &Path, kind: MediaKind) -> StrictOwner<'_> {
        let mut best: Option<(usize, &Library)> = None;
        let mut ambiguous = false;

        for res in self.libraries.iter().filter(|r| r.library.kind == kind) {
            let Some(depth) = res
                .resolved_roots
                .iter()
                .filter(|root| resolved_path.starts_with(root))
                .map(|root| root.components().count())
                .max()
            else {
                continue;
            };

            match best {
                Some((best_depth, _)) if best_depth > depth => {}
                Some((best_depth, best_lib)) if best_depth == depth => {
                    if best_lib.id != res.library.id {
                        ambiguous = true;
                    }
                }
                _ => {
                    best = Some((depth, &res.library));
                    ambiguous = false;
                }
            }
        }

        if ambiguous {
            StrictOwner::Ambiguous
        } else if let Some((_, lib)) = best {
            StrictOwner::Owned(lib)
        } else {
            StrictOwner::Unmatched
        }
    }

    /// Check if the scope is still structurally and temporally identical to the current snapshot.
    ///
    /// Verifies count, id, kind, root_paths, and accepts_work status.
    pub fn matches(&self, current: &LibraryOwnership) -> bool {
        if self.libraries.len() != current.libraries().len() {
            return false;
        }

        for res in &self.libraries {
            let Some(curr) = current.library(&res.library.id) else {
                return false;
            };
            if curr.kind != res.library.kind || curr.root_paths != res.library.root_paths {
                return false;
            }
            if self.accepts_work(&res.library.id) != current.accepts_work(&res.library.id) {
                return false;
            }
        }

        true
    }

    /// Check whether a library accepts work in this scope snapshot.
    pub fn accepts_work(&self, library_id: &str) -> bool {
        self.libraries.iter().any(|r| r.library.id == library_id)
            && !self.blocked.contains(library_id)
    }

    /// Returns the libraries in this scope.
    pub fn libraries(&self) -> impl Iterator<Item = &Library> {
        self.libraries.iter().map(|r| &r.library)
    }
}
