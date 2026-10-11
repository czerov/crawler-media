//! Request-local Library ownership snapshot: one config + roots read, then a
//! pure longest-root-prefix decision per path (no IO, no stat, no canonicalize).
//!
//! This module owns the single implementation of the ownership rule used by
//! both [`LibraryOwnership::owner`] and `Store::library_for_path`.

use std::collections::HashSet;
use std::path::Path;

use domain::MediaKind;

use super::Library;

/// Outcome of the pure longest-root match for one path.
enum OwnerMatch<'a> {
    Unique(&'a Library),
    Ambiguous,
    Unmatched,
}

/// Longest-root-prefix match, same rule as the historical `library_for_path`:
/// same kind only, the largest matching root component count wins, an equal
/// depth reached by two different libraries is ambiguous, and a path that
/// matches no root is `Unmatched`. `Path::starts_with` compares whole
/// components, so `/owned-other` is not owned by a `/owned` root.
fn match_owner<'a>(libraries: &'a [Library], path: &Path, kind: MediaKind) -> OwnerMatch<'a> {
    let mut best: Option<(usize, &'a Library)> = None;
    let mut ambiguous = false;
    for library in libraries.iter().filter(|library| library.kind == kind) {
        let Some(depth) = library
            .root_paths
            .iter()
            .filter(|root| path.starts_with(root))
            .map(|root| root.components().count())
            .max()
        else {
            continue;
        };
        match best {
            Some((best_depth, _)) if best_depth > depth => {}
            Some((best_depth, best_library)) if best_depth == depth => {
                if best_library.id != library.id {
                    ambiguous = true;
                }
            }
            _ => {
                best = Some((depth, library));
                ambiguous = false;
            }
        }
    }
    if ambiguous {
        OwnerMatch::Ambiguous
    } else if let Some((_, library)) = best {
        OwnerMatch::Unique(library)
    } else {
        OwnerMatch::Unmatched
    }
}

/// The kind's default library, in the snapshot's `(sort_order, name)` order.
fn default_for<'a>(libraries: &'a [Library], kind: MediaKind) -> Option<&'a Library> {
    libraries
        .iter()
        .find(|library| library.kind == kind && library.is_default)
}

/// Historical ownership with the default fallback: only a complete miss falls
/// back to the kind's default; ambiguity stays unresolved.
pub(crate) fn owner_with_default<'a>(
    libraries: &'a [Library],
    path: &Path,
    kind: MediaKind,
) -> Option<&'a Library> {
    match match_owner(libraries, path, kind) {
        OwnerMatch::Unique(library) => Some(library),
        OwnerMatch::Ambiguous => None,
        OwnerMatch::Unmatched => default_for(libraries, kind),
    }
}

/// A request-local view of Library configuration, roots, and blocked ids.
#[derive(Clone, Debug)]
pub struct LibraryOwnership {
    libraries: Vec<Library>,
    blocked: HashSet<String>,
}

impl LibraryOwnership {
    pub fn from_parts(libraries: Vec<Library>, blocked: HashSet<String>) -> Self {
        Self { libraries, blocked }
    }

    pub fn libraries(&self) -> &[Library] {
        &self.libraries
    }

    pub fn library(&self, id: &str) -> Option<&Library> {
        self.libraries.iter().find(|library| library.id == id)
    }

    pub fn default_for(&self, kind: MediaKind) -> Option<&Library> {
        default_for(&self.libraries, kind)
    }

    /// Longest-root owner for `path`, falling back to the kind's default only
    /// when nothing matched at all; ambiguity yields `None`.
    pub fn owner(&self, path: &Path, kind: MediaKind) -> Option<&Library> {
        owner_with_default(&self.libraries, path, kind)
    }

    /// Same as `Store::library_accepts_work`: an existing library with no
    /// `library_removals` row accepts work. The removal row blocks regardless
    /// of state, including `succeeded`.
    pub fn accepts_work(&self, id: &str) -> bool {
        self.library(id).is_some() && !self.blocked.contains(id)
    }
}
