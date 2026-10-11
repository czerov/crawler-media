use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

use super::ProbeManager;
use crate::fingerprint_job::CaptureGate;

#[derive(Clone)]
pub struct MetadataPriorityGate {
    metadata_pending: Arc<AtomicUsize>,
    metadata_idle: Arc<Notify>,
    store: Arc<parking_lot::Mutex<store::Store>>,
    registry: Option<Arc<crate::library_lifecycle::LibraryWorkRegistry>>,
    owners: Option<Arc<std::collections::HashMap<String, (String, marker::WorkControl)>>>,
}

impl MetadataPriorityGate {
    pub fn new(manager: Arc<ProbeManager>) -> Self {
        Self {
            metadata_pending: manager.metadata_pending.clone(),
            metadata_idle: manager.metadata_idle.clone(),
            store: manager.store.clone(),
            registry: manager.library_work.lock().clone(),
            owners: None,
        }
    }

    pub fn from_manager(manager: &ProbeManager) -> Self {
        Self {
            metadata_pending: manager.metadata_pending.clone(),
            metadata_idle: manager.metadata_idle.clone(),
            store: manager.store.clone(),
            registry: manager.library_work.lock().clone(),
            owners: None,
        }
    }

    /// Retain one lease per original strict owner for the entire expanded pipeline.
    pub(super) fn for_rows(
        manager: &ProbeManager,
        units: &[super::ProbeUnit],
    ) -> Result<(Self, Vec<crate::library_lifecycle::LibraryWorkLease>), String> {
        let mut gate = Self::from_manager(manager);
        let Some(registry) = gate.registry.as_ref() else {
            return Ok((gate, Vec::new()));
        };
        let owners = {
            let store = manager.store.lock();
            units
                .iter()
                .map(|unit| {
                    let current = store
                        .get_ledger(&unit.row.id.to_string())
                        .map_err(|e| e.to_string())?
                        .ok_or("file_deleted: probe ledger missing")?;
                    if current.path != unit.row.path || current.media_id != unit.row.media_id {
                        return Err("Library probe source identity changed".to_string());
                    }
                    let owner = store
                        .library_for_path_strict(std::path::Path::new(&unit.row.path), unit.kind)
                        .map_err(|e| e.to_string())?
                        .ok_or("Library no longer owns probe source")?;
                    Ok((unit.row.id.to_string(), owner.id))
                })
                .collect::<Result<Vec<_>, String>>()?
        };
        let mut leases = Vec::new();
        let mut controls = std::collections::HashMap::new();
        for (_, owner) in &owners {
            if !controls.contains_key(owner) {
                let lease = registry.acquire(&manager.store, owner)?;
                controls.insert(owner.clone(), lease.control().signal().clone());
                leases.push(lease);
            }
        }
        gate.owners = Some(Arc::new(
            owners
                .into_iter()
                .map(|(row, owner)| {
                    let control = controls[&owner].clone();
                    (row, (owner, control))
                })
                .collect(),
        ));
        for unit in units {
            if gate.work_control(&unit.row).is_cancelled() {
                return Err("Library probe source cancelled or changed".into());
            }
        }
        Ok((gate, leases))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn expanded_owners_remain_leased_and_cancel_independently() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(parking_lot::Mutex::new(
            store::Store::open(temp.path()).unwrap(),
        ));
        let manager = ProbeManager::new(store.clone());
        let registry = Arc::new(crate::library_lifecycle::LibraryWorkRegistry::default());
        *manager.library_work.lock() = Some(registry.clone());
        let media = domain::Media {
            id: domain::MediaId::new(),
            kind: domain::MediaKind::Tv,
            title: "Fixture".into(),
            year: None,
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.lock().insert_media(&media).unwrap();
        let mut units = Vec::new();
        let mut libraries = Vec::new();
        for name in ["a", "b"] {
            let root = temp.path().join(name);
            let owner = store
                .lock()
                .create_library(
                    media.kind,
                    name,
                    &[root.to_str().unwrap()],
                    "everyone",
                    true,
                    &[],
                )
                .unwrap();
            let row = domain::LedgerRow {
                id: domain::LedgerId::new(),
                media_id: media.id,
                path: root.join("episode.mkv").to_string_lossy().into_owned(),
                season: Some(1),
                episode: Some(1),
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: domain::QualitySource::Release,
                confidence: domain::Confidence::High,
                filter_score: None,
            };
            store.lock().insert_ledger(&row).unwrap();
            units.push(super::super::ProbeUnit {
                row,
                kind: media.kind,
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: true,
                reuse_media_info_cache: true,
                marker_refresh_id: None,
                job_id: None,
            });
            libraries.push(owner);
        }
        let unleased = MetadataPriorityGate::from_manager(&manager);
        assert!(unleased.work_control(&units[0].row).is_cancelled());
        let (gate, leases) = MetadataPriorityGate::for_rows(&manager, &units).unwrap();
        assert_eq!(leases.len(), 2);
        assert!(!gate.work_control(&units[1].row).is_cancelled());
        registry.cancel(&libraries[1].id);
        assert!(gate.work_control(&units[1].row).is_cancelled());
        assert!(!gate.work_control(&units[0].row).is_cancelled());
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(10),
                registry.wait_idle(&libraries[1].id)
            )
            .await
            .is_err()
        );
        drop(leases);
        registry.wait_idle(&libraries[1].id).await;
    }
}

impl CaptureGate for MetadataPriorityGate {
    fn work_control(&self, row: &domain::LedgerRow) -> marker::WorkControl {
        let owner = {
            let store = self.store.lock();
            let current = store.get_ledger(&row.id.to_string()).ok().flatten();
            current
                .filter(|current| current.path == row.path && current.media_id == row.media_id)
                .and_then(|_| store.get_media(row.media_id).ok().flatten())
                .and_then(|media| {
                    store
                        .library_for_path_strict(std::path::Path::new(&row.path), media.kind)
                        .ok()
                        .flatten()
                })
        };
        // A registry-backed capture without an active original lease fails closed.
        // Never switch an old pipeline to a new Library at the same path.
        if self.registry.is_none() {
            return marker::WorkControl::default();
        }
        if let Some(owners) = &self.owners {
            if let Some((original, control)) = owners.get(&row.id.to_string()) {
                if owner.as_ref().is_some_and(|owner| &owner.id == original) {
                    return control.clone();
                }
            }
        } else if let Some(control) = owner.and_then(|owner| {
            self.registry
                .as_ref()
                .and_then(|registry| registry.active_control(&owner.id))
        }) {
            return control.signal().clone();
        }
        let cancelled = marker::WorkControl::default();
        cancelled.cancel();
        cancelled
    }
    fn wait_before_capture(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            loop {
                let notified = self.metadata_idle.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.metadata_pending.load(Ordering::Acquire) == 0 {
                    return;
                }
                notified.await;
            }
        })
    }
}
