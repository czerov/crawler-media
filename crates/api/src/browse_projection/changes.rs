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

    fn retain_libraries(&mut self, library_ids: &HashSet<String>) {
        self.media_by_library.retain(|id, _| library_ids.contains(id));
        self.ledger_by_library.retain(|id, _| library_ids.contains(id));
        self.paths_by_library.retain(|id, _| library_ids.contains(id));
        self.catalog_by_library.retain(|id, _| library_ids.contains(id));
    }
}

pub struct ChangeConsumer {
    deps: Arc<Mutex<ProjectionDependencies>>,
}

impl ChangeConsumer {
    pub fn new(deps: Arc<Mutex<ProjectionDependencies>>) -> Self {
        Self { deps }
    }

    /// Load persisted ready projections once their reverse-dependency coverage is absent.
    pub fn hydrate_ready_dependencies(&self, store: &Store) -> Result<(), StoreError> {
        let ownership = store.library_ownership()?;
        let mut dependencies = self.deps.lock();
        let current_ids = ownership
            .libraries()
            .into_iter()
            .map(|library| library.id.clone())
            .collect();
        dependencies.retain_libraries(&current_ids);
        for library in ownership.libraries() {
            if dependencies.catalog_by_library.contains_key(&library.id) {
                continue;
            }
            let Some(snapshot) = store.browse_projection(&library.id)? else {
                dependencies.record_library(&library.id, [], [], [], []);
                continue;
            };
            let mut media_ids = HashSet::new();
            let mut ledger_ids = HashSet::new();
            let mut paths = HashSet::new();
            for file in snapshot.files {
                if let Some(facts) = super::facts::FileFacts::from_json(&file.facts) {
                    media_ids.insert(facts.row.media_id);
                    ledger_ids.insert(facts.row.id);
                    paths.insert(facts.row.path);
                }
            }
            let catalog_keys = snapshot.media.iter().filter_map(|media| {
                let facts = media.facts.get("media")?;
                let tmdb_id = facts.get("tmdb_id")?.as_str()?;
                let kind = facts.get("kind")?.as_str().unwrap_or("movie");
                Some(format!("tmdb:/{kind}/{tmdb_id}"))
            });
            dependencies.record_library(&library.id, media_ids, ledger_ids, paths, catalog_keys);
        }
        Ok(())
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
            // Retry a coalescing sentinel retained when acknowledgement committed before pruning.
            if cursor > 0 {
                let acknowledged = store.browse_changes(source, cursor - 1, 1)?;
                if acknowledged.last().is_some_and(|change| {
                    change.seq == cursor && change.entity_kind == "browse_all"
                }) {
                    let affected = self.resolve_affected(store, ownership, &acknowledged);
                    store.apply_browse_changes(source, cursor, &affected)?;
                    *budget_remaining = budget_remaining.saturating_sub(1);
                    return Ok((1, true));
                }
            }
            store.apply_browse_changes(source, cursor, &[])?;
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
        let mut deps = self.deps.lock();
        deps.retain_libraries(&all_libs.iter().map(|library| library.id.clone()).collect());
        for c in changes {
            match c.source {
                BrowseChangeSource::App => match c.entity_kind.as_str() {
                    "library" => {
                        // A new library has no persisted projection dependencies yet.
                        if deps.media_by_library.contains_key(&c.entity_key)
                            || deps.ledger_by_library.contains_key(&c.entity_key)
                        {
                            affected.insert(c.entity_key.clone());
                        } else {
                            tracing::debug!(library_id = %c.entity_key, "新 Library 不引用现有投影依赖");
                        }
                    }
                    "library_root" => {
                        affected.insert(c.entity_key.clone());
                        for root in c.old_path.iter().chain(c.new_path.iter()) {
                            let root_kind = all_libs
                                .iter()
                                .find(|library| library.id == c.entity_key)
                                .map(|library| library.kind);
                            for (lib_id, paths) in &deps.paths_by_library {
                                for path in paths.iter().filter(|path| {
                                    Path::new(path).starts_with(root)
                                        || Path::new(root).starts_with(path)
                                }) {
                                    affected.insert(lib_id.clone());
                                    if let Some(kind) = root_kind {
                                        if let Some(owner) = ownership.owner(Path::new(path), kind) {
                                            affected.insert(owner.id.clone());
                                        }
                                    }
                                }
                            }
                            if let Some(kind) = root_kind {
                                if let Some(owner) = ownership.owner(Path::new(root), kind) {
                                    affected.insert(owner.id.clone());
                                } else if let Some(default) = ownership.default_for(kind) {
                                    affected.insert(default.id.clone());
                                }
                            }
                        }
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
                            if !found_any {
                                tracing::debug!(media_id = %media_id, "浏览投影变更不引用已发布依赖");
                            }
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
                        // Resolve the changed row against published dependencies first.
                        if let Ok(lid) = c.entity_key.parse::<LedgerId>() {
                            for (lib_id, ledgers) in &deps.ledger_by_library {
                                if ledgers.contains(&lid) {
                                    affected.insert(lib_id.clone());
                                    resolved_any = true;
                                }
                            }
                        }
                        if let Some(old) = &c.old_path {
                            for (lib_id, paths) in &deps.paths_by_library {
                                if paths.contains(old) {
                                    affected.insert(lib_id.clone());
                                    resolved_any = true;
                                }
                            }
                        }
                        if let Some(new_p) = &c.new_path {
                            let actual_kind = store
                                .ledger_by_path(new_p)
                                .ok()
                                .flatten()
                                .and_then(|row| store.get_media(row.media_id).ok().flatten())
                                .map(|media| media.kind);
                            if let Some(kind) = actual_kind {
                                if let Some(owner) = ownership.owner(Path::new(new_p), kind) {
                                    affected.insert(owner.id.clone());
                                    resolved_any = true;
                                }
                            } else {
                                for kind in [domain::MediaKind::Movie, domain::MediaKind::Tv] {
                                    if let Some(owner) = ownership.owner(Path::new(new_p), kind) {
                                        affected.insert(owner.id.clone());
                                        resolved_any = true;
                                    }
                                }
                            }
                        }
                        if !resolved_any {
                            if c.new_path.is_none() {
                                if let Some(old) = &c.old_path {
                                    let mut owner_found = false;
                                    for lib in all_libs {
                                        if let Some(owner) = ownership.owner(Path::new(old), lib.kind) {
                                            affected.insert(owner.id.clone());
                                            owner_found = true;
                                        }
                                    }
                                    if !owner_found {
                                        affected.extend(all_libs.iter().map(|l| l.id.clone()));
                                    }
                                } else {
                                    tracing::debug!(entity_key = %c.entity_key, "浏览投影 ledger 变更不引用已发布依赖");
                                }
                            } else {
                                tracing::debug!(entity_key = %c.entity_key, "浏览投影 ledger 变更不引用已发布依赖");
                            }
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
                            tracing::debug!(entity_key = %c.entity_key, "浏览投影 file_meta 变更不引用已发布依赖");
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
                            tracing::debug!(entity_key = %c.entity_key, "浏览投影 marker 变更不引用已发布依赖");
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
                        let known_detail_key = base_key
                            .split_once(':')
                            .is_some_and(|(_, path)| {
                                path.starts_with('/')
                                    && path.split('/').filter(|part| !part.is_empty()).count() == 2
                            });
                        // A matching in-memory dependency proves only that library's
                        // relationship. Missing entries are unknown after restart, not
                        // evidence that a ready persisted projection is unrelated.
                        // Libraries without persisted/in-memory coverage remain unknown.
                        for lib in all_libs {
                            if !deps.catalog_by_library.contains_key(&lib.id) {
                                affected.insert(lib.id.clone());
                            }
                        }
                        if !resolved && (!known_detail_key || all_libs.iter().any(|lib| {
                            !deps.catalog_by_library.contains_key(&lib.id)
                        })) {
                            affected.extend(all_libs.iter().map(|lib| lib.id.clone()));
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
