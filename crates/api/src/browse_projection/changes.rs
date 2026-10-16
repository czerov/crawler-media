use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use domain::{LedgerId, MediaId};
use parking_lot::Mutex;
use store::{BrowseChange, BrowseChangeSource, LibraryOwnership, Store, StoreError};

use crate::management::ApiState;

pub const MAX_EVENTS_PER_ROUND: usize = 512;

/// In-memory cache of projected library dependencies for fast reverse-invalidation.
#[derive(Default)]
pub struct ProjectionDependencies {
    /// library_id -> set of media_ids currently projected in that library
    media_by_library: HashMap<String, HashSet<MediaId>>,
    /// library_id -> set of ledger_ids currently projected in that library
    ledger_by_library: HashMap<String, HashSet<LedgerId>>,
    /// library_id -> set of paths currently projected in that library
    paths_by_library: HashMap<String, HashSet<String>>,
    /// library_id -> set of catalog cache_keys (e.g. "tmdb:movie:123") projected
    catalog_by_library: HashMap<String, HashSet<String>>,
}

impl ProjectionDependencies {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_library(
        &mut self,
        library_id: &str,
        media_ids: impl IntoIterator<Item = MediaId>,
        ledger_ids: impl IntoIterator<Item = LedgerId>,
        paths: impl IntoIterator<Item = String>,
        catalog_keys: impl IntoIterator<Item = String>,
    ) {
        self.media_by_library
            .insert(library_id.to_string(), media_ids.into_iter().collect());
        self.ledger_by_library
            .insert(library_id.to_string(), ledger_ids.into_iter().collect());
        self.paths_by_library
            .insert(library_id.to_string(), paths.into_iter().collect());
        self.catalog_by_library
            .insert(library_id.to_string(), catalog_keys.into_iter().collect());
    }

    pub fn remove_library(&mut self, library_id: &str) {
        self.media_by_library.remove(library_id);
        self.ledger_by_library.remove(library_id);
        self.paths_by_library.remove(library_id);
        self.catalog_by_library.remove(library_id);
    }
}

pub struct ChangeConsumer {
    deps: Arc<Mutex<ProjectionDependencies>>,
}

impl ChangeConsumer {
    pub fn new(deps: Arc<Mutex<ProjectionDependencies>>) -> Self {
        Self { deps }
    }

    /// Poll changes from one source with a caller-supplied event budget.
    /// Updates `budget_remaining` and returns `(events_processed, is_complete)`.
    pub fn poll_source_budgeted(
        &self,
        store: &Store,
        ownership: &LibraryOwnership,
        source: BrowseChangeSource,
        budget_remaining: &mut usize,
    ) -> Result<(usize, bool), StoreError> {
        if *budget_remaining == 0 {
            return Ok((0, false));
        }
        let cursor = store.browse_change_cursor(source)?;
        let changes = store.browse_changes(source, cursor, *budget_remaining)?;
        if changes.is_empty() {
            return Ok((0, true));
        }

        let is_complete = changes.len() < *budget_remaining;
        *budget_remaining = budget_remaining.saturating_sub(changes.len());
        let last_seq = changes.last().map(|c| c.seq).unwrap_or(cursor);

        let affected = self.resolve_affected(store, ownership, &changes);
        store.apply_browse_changes(source, last_seq, &affected)?;

        Ok((changes.len(), is_complete))
    }

    /// Poll changes from one source using MAX_EVENTS_PER_ROUND.
    /// Returns `(events_processed, is_complete)`.
    pub fn poll_source(
        &self,
        store: &Store,
        ownership: &LibraryOwnership,
        source: BrowseChangeSource,
    ) -> Result<(usize, bool), StoreError> {
        let mut budget = MAX_EVENTS_PER_ROUND;
        self.poll_source_budgeted(store, ownership, source, &mut budget)
    }

    /// Resolves which libraries are affected by a batch of changes.
    pub fn resolve_affected(
        &self,
        store: &Store,
        ownership: &LibraryOwnership,
        changes: &[BrowseChange],
    ) -> Vec<String> {
        let all_libs = ownership.libraries();
        let mut affected = HashSet::new();
        let deps = self.deps.lock();

        for c in changes {
            match c.source {
                BrowseChangeSource::App => match c.entity_kind.as_str() {
                    "library" => {
                        // Library settings changed: this library is affected
                        affected.insert(c.entity_key.clone());
                    }
                    "library_root" => {
                        // Library roots changed: affects the library or all libraries if unknown
                        affected.extend(all_libs.iter().map(|l| l.id.clone()));
                    }
                    "media" => {
                        if let Ok(media_id) = c.entity_key.parse::<MediaId>() {
                            let mut found_any = false;
                            for (lib_id, media_set) in &deps.media_by_library {
                                if media_set.contains(&media_id) {
                                    affected.insert(lib_id.clone());
                                    found_any = true;
                                }
                            }
                            // Conservative invalidation if not found in cached deps
                            if !found_any {
                                affected.extend(all_libs.iter().map(|l| l.id.clone()));
                            }
                        } else {
                            // Unknown key conservatively dirties all
                            affected.extend(all_libs.iter().map(|l| l.id.clone()));
                        }
                    }
                    _ => {
                        // Unknown kind: conservatively dirty all
                        affected.extend(all_libs.iter().map(|l| l.id.clone()));
                    }
                },
                BrowseChangeSource::Library => match c.entity_kind.as_str() {
                    "ledger" => {
                        // A move or delete/insert dirties old owner and new owner
                        let mut resolved_any = false;
                        if let Some(old) = &c.old_path {
                            resolved_any = true;
                            // Check old path in current ownership
                            for lib in all_libs {
                                if let Some(owner) = ownership.owner(Path::new(old), lib.kind) {
                                    affected.insert(owner.id.clone());
                                }
                            }
                            // Check old path in recorded deps
                            for (lib_id, paths) in &deps.paths_by_library {
                                if paths.contains(old) {
                                    affected.insert(lib_id.clone());
                                }
                            }
                        }
                        if let Some(new_p) = &c.new_path {
                            resolved_any = true;
                            for lib in all_libs {
                                if let Some(owner) = ownership.owner(Path::new(new_p), lib.kind) {
                                    affected.insert(owner.id.clone());
                                }
                            }
                        }
                        if !resolved_any {
                            // If neither path was provided, check ledger_id in deps or conservative
                            if let Ok(lid) = c.entity_key.parse::<LedgerId>() {
                                for (lib_id, ledgers) in &deps.ledger_by_library {
                                    if ledgers.contains(&lid) {
                                        affected.insert(lib_id.clone());
                                        resolved_any = true;
                                    }
                                }
                            }
                        }
                        if !resolved_any {
                            affected.extend(all_libs.iter().map(|l| l.id.clone()));
                        }
                    }
                    "file_meta" => {
                        let mut resolved = false;
                        if let Ok(lid) = c.entity_key.parse::<LedgerId>() {
                            for (lib_id, ledgers) in &deps.ledger_by_library {
                                if ledgers.contains(&lid) {
                                    affected.insert(lib_id.clone());
                                    resolved = true;
                                }
                            }
                        }
                        if !resolved {
                            affected.extend(all_libs.iter().map(|l| l.id.clone()));
                        }
                    }
                    "media_marker" => {
                        // Format: marker:media_id:season:episode
                        // Invalidation: if season/episode are '*', invalidates entire media
                        let mut resolved = false;
                        if let Some(rest) = c.entity_key.strip_prefix("marker:") {
                            let parts: Vec<&str> = rest.split(':').collect();
                            if let Some(media_id_str) = parts.first() {
                                if let Ok(mid) = media_id_str.parse::<MediaId>() {
                                    for (lib_id, media_set) in &deps.media_by_library {
                                        if media_set.contains(&mid) {
                                            affected.insert(lib_id.clone());
                                            resolved = true;
                                        }
                                    }
                                }
                            }
                        }
                        if !resolved {
                            affected.extend(all_libs.iter().map(|l| l.id.clone()));
                        }
                    }
                    _ => {
                        affected.extend(all_libs.iter().map(|l| l.id.clone()));
                    }
                },
                BrowseChangeSource::Catalog => match c.entity_kind.as_str() {
                    "catalog_cache" => {
                        let key = &c.entity_key;
                        // Catalog search/discover queries explicitly do NOT affect library projections!
                        if key.contains(":search:")
                            || key.contains(":discover:")
                            || key.contains("/search")
                            || key.contains("/discover")
                        {
                            continue;
                        }

                        // Normalize key to base prefix before query parameters (e.g. tmdb:/movie/123?language=zh-CN -> tmdb:/movie/123)
                        let base_key = key.split('?').next().unwrap_or(key);

                        let mut resolved = false;
                        for (lib_id, catalog_keys) in &deps.catalog_by_library {
                            if catalog_keys.iter().any(|k| {
                                let k_base = k.split('?').next().unwrap_or(k);
                                base_key == k_base || key.starts_with(k) || k.starts_with(base_key)
                            }) {
                                affected.insert(lib_id.clone());
                                resolved = true;
                            }
                        }
                        // If it's a detail cache and not found in deps, conservatively dirty all
                        if !resolved {
                            affected.extend(all_libs.iter().map(|l| l.id.clone()));
                        }
                    }
                    _ => {
                        affected.extend(all_libs.iter().map(|l| l.id.clone()));
                    }
                },
            }
        }

        affected.into_iter().collect()
    }
}
