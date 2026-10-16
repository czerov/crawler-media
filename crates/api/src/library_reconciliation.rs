//! Pure filesystem reconciliation helper.
//!
//! Enforces a depth limit of 5 and a 100,000-entry budget. Symlinks are never
//! followed out of scope. Incomplete scans are reported honestly via `incomplete`,
//! and results are pure filesystem candidates, NOT authorization decisions.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Candidate paths and statistics discovered by indexing directory trees.
#[derive(Clone, Debug, Default)]
pub struct ReconciliationIndex {
    pub candidates: std::collections::HashMap<OsString, Vec<PathBuf>>,
    pub examined: usize,
    pub incomplete: bool,
}

/// Recursively index file basenames within roots with bounded depth and work budget.
///
/// Rules:
/// - Scans each root once
/// - Depth limit of 5 (relative to root)
/// - Maximum of `max_entries` filesystem entries inspected
/// - Maximum work budget of 30 seconds
/// - Returns `examined` and `incomplete` honestly
/// - Never follows symlinks that escape the root scope
pub fn index_basenames(
    roots: &[PathBuf],
    control: &library::WorkControl,
    max_entries: usize,
) -> Result<ReconciliationIndex, String> {
    control.check().map_err(|e| e.to_string())?;

    let start = Instant::now();
    let timeout = std::time::Duration::from_secs(30);

    let mut index = ReconciliationIndex::default();
    let mut visited_dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    for root in roots {
        if !root.is_dir() {
            continue;
        }

        let canonical_root = match std::fs::canonicalize(root) {
            Ok(c) => c,
            Err(_) => {
                index.incomplete = true;
                continue;
            }
        };

        if !visited_dirs.insert(canonical_root.clone()) {
            continue;
        }

        let mut queue = std::collections::VecDeque::new();
        queue.push_back((root.clone(), canonical_root.clone(), 0usize));
        while let Some((dir, current_canonical, depth)) = queue.pop_front() {
            if index.examined >= max_entries || start.elapsed() >= timeout {
                index.incomplete = true;
                return Ok(index);
            }

            if let Err(e) = control.check() {
                return Err(e.to_string());
            }

            // Verify containment at the moment of traversal
            let verified_canonical = match std::fs::canonicalize(&dir) {
                Ok(c) if c.starts_with(&canonical_root) => c,
                _ => {
                    index.incomplete = true;
                    continue;
                }
            };

            let read_dir_result = std::fs::read_dir(&dir);
            let read_dir_entries = match read_dir_result {
                Ok(entries) => entries,
                Err(_) => {
                    index.incomplete = true;
                    continue;
                }
            };

            let cont = process_dir_entries(
                read_dir_entries,
                &canonical_root,
                depth,
                max_entries,
                start,
                timeout,
                &mut index,
                &mut visited_dirs,
                &mut queue,
            );
            if !cont {
                return Ok(index);
            }
        }
    }
    Ok(index)
}

#[allow(clippy::too_many_arguments)]
fn process_dir_entries(
    read_dir_entries: std::fs::ReadDir,
    canonical_root: &Path,
    depth: usize,
    max_entries: usize,
    start: Instant,
    timeout: std::time::Duration,
    index: &mut ReconciliationIndex,
    visited_dirs: &mut std::collections::HashSet<PathBuf>,
    queue: &mut std::collections::VecDeque<(PathBuf, PathBuf, usize)>,
) -> bool {
    for entry_res in read_dir_entries {
        if index.examined >= max_entries || start.elapsed() >= timeout {
            index.incomplete = true;
            return false;
        }

        index.examined += 1;

        let entry = match entry_res {
            Ok(e) => e,
            Err(_) => {
                index.incomplete = true;
                continue;
            }
        };

        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => {
                index.incomplete = true;
                continue;
            }
        };

        if file_type.is_dir() {
            if depth < 5 {
                let next_canonical = match std::fs::canonicalize(&path) {
                    Ok(c) => c,
                    Err(_) => {
                        index.incomplete = true;
                        continue;
                    }
                };
                if next_canonical.starts_with(canonical_root) && visited_dirs.insert(next_canonical.clone()) {
                    // Traverse the raw path so discovered candidates keep the path as it
                    // exists under the root; the canonical form is only for containment
                    // verification and the visited-directory set.
                    queue.push_back((path.clone(), next_canonical, depth + 1));
                }
            }
        } else if file_type.is_file() {
            if let Ok(canon_file) = std::fs::canonicalize(&path) {
                if canon_file.starts_with(canonical_root) {
                    if let Some(name) = path.file_name() {
                        index
                            .candidates
                            .entry(name.to_os_string())
                            .or_default()
                            .push(path);
                    }
                }
            } else {
                index.incomplete = true;
            }
        } else if file_type.is_symlink() {
            let Ok(target) = std::fs::canonicalize(&path) else {
                index.incomplete = true;
                continue;
            };
            if !target.starts_with(canonical_root) {
                continue;
            }
            if target.is_dir() {
                if depth < 5 && visited_dirs.insert(target.clone()) {
                    queue.push_back((target.clone(), target, depth + 1));
                }
            } else if target.is_file() {
                if let Some(name) = path.file_name() {
                    index
                        .candidates
                        .entry(name.to_os_string())
                        .or_default()
                        .push(path);
                }
            }
        }
    }
    true
}
