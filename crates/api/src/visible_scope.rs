//! Scoped visible media batch resolution.

use std::collections::HashSet;

use domain::{MediaId, UserId};
use store::{LibraryPathScope, Store, StoreError, StrictOwner};

pub struct VisibleScope {
    pub media_ids: Vec<MediaId>,
}

impl VisibleScope {
    pub fn load(store: &Store, user_id: UserId) -> Result<Self, StoreError> {
        let snapshot = store.library_ownership()?;
        let visible_libs: HashSet<String> = snapshot
            .libraries()
            .iter()
            .filter(|lib| crate::http::library::library_visible(store, lib, Some(user_id)))
            .map(|lib| lib.id.clone())
            .collect();

        let scope = LibraryPathScope::resolve(snapshot)?;
        let rows = store.list_ledger()?;
        let media_ids: Vec<MediaId> = rows.iter().map(|r| r.media_id).collect();
        let media_map = store.media_batch(&media_ids)?;

        let mut media_set = HashSet::new();
        for row in rows {
            let Some(media) = media_map.get(&row.media_id) else {
                continue;
            };
            let Ok(resolved) = LibraryPathScope::resolve_path(std::path::Path::new(&row.path)) else {
                continue;
            };
            match scope.owner(&resolved, media.kind) {
                StrictOwner::Owned(owner) if visible_libs.contains(&owner.id) => {
                    media_set.insert(row.media_id);
                }
                _ => {}
            }
        }

        Ok(Self {
            media_ids: media_set.into_iter().collect(),
        })
    }

    pub fn media_ids(&self) -> &[MediaId] {
        &self.media_ids
    }
}
