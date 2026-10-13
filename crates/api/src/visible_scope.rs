//! Scoped visible media batch resolution.

use std::collections::HashSet;

use domain::{MediaId, UserId};
use store::{LibraryOwnership, Store, StoreError};

pub struct VisibleScope {
    pub media_ids: Vec<MediaId>,
}

impl VisibleScope {
    pub fn load(store: &Store, user_id: UserId) -> Result<Self, StoreError> {
        let ownership = store.library_ownership()?;
        let visible_libs: HashSet<String> = ownership
            .libraries()
            .iter()
            .filter(|lib| crate::http::library::library_visible(store, lib, Some(user_id)))
            .map(|lib| lib.id.clone())
            .collect();

        let mut media_set = HashSet::new();
        for row in store.list_ledger()? {
            let p = std::path::Path::new(&row.path);
            if let Some(owner) = ownership.owner(p, domain::MediaKind::Movie) {
                if visible_libs.contains(&owner.id) {
                    media_set.insert(row.media_id);
                    continue;
                }
            }
            if let Some(owner) = ownership.owner(p, domain::MediaKind::Tv) {
                if visible_libs.contains(&owner.id) {
                    media_set.insert(row.media_id);
                }
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
