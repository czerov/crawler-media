use std::collections::HashMap;

use domain::{LedgerId, LedgerRow, MediaKind, Release, Subscribe};
use subscribe::SubscribeFacts;

use crate::browse_projection::ReadRevisions;
use crate::library_ownership::LibraryOwnership;
use crate::{Store, StoreError};

/// Owned database snapshot for out-of-lock Subscribe library evaluation.
#[derive(Clone, Debug)]
pub struct SubscribeLibrarySnapshot {
    pub subscribe: Subscribe,
    pub kind: MediaKind,
    pub ownership: LibraryOwnership,
    pub saved: SubscribeFacts,
    pub rows: Vec<LedgerRow>,
    pub qualities: HashMap<LedgerId, Result<Release, String>>,
    pub revisions: ReadRevisions,
}

impl Store {
    /// Collect the owned database facts needed to resolve Subscribe facts
    /// without holding the Store lock during filesystem resolution.
    pub fn subscribe_library_snapshot(
        &self,
        subscribe: &Subscribe,
        kind: MediaKind,
    ) -> Result<SubscribeLibrarySnapshot, StoreError> {
        let saved = self.load_subscribe_facts(subscribe.id)?;
        let ownership = self.library_ownership()?;
        let revisions = self.read_revisions()?;
        let rows = self.ledger_for_media(subscribe.media_id)?;
        let mut qualities = HashMap::new();
        for row in &rows {
            qualities.insert(row.id, self.owned_quality(row).map_err(|e| e.to_string()));
        }

        Ok(SubscribeLibrarySnapshot {
            subscribe: subscribe.clone(),
            kind,
            ownership,
            saved,
            rows,
            qualities,
            revisions,
        })
    }
}
