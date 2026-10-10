use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

use crate::fingerprint_job::CaptureGate;
use super::ProbeManager;

#[derive(Clone)]
pub struct MetadataPriorityGate {
    metadata_pending: Arc<AtomicUsize>,
    metadata_idle: Arc<Notify>,
    store: Arc<parking_lot::Mutex<store::Store>>,
    registry: Option<Arc<crate::library_lifecycle::LibraryWorkRegistry>>,
}

impl MetadataPriorityGate {
    pub fn new(manager: Arc<ProbeManager>) -> Self {
        Self {
            metadata_pending: manager.metadata_pending.clone(),
            metadata_idle: manager.metadata_idle.clone(),
            store: manager.store.clone(),
            registry: manager.library_work.lock().clone(),
        }
    }

    pub fn from_manager(manager: &ProbeManager) -> Self {
        Self {
            metadata_pending: manager.metadata_pending.clone(),
            metadata_idle: manager.metadata_idle.clone(),
            store: manager.store.clone(),
            registry: manager.library_work.lock().clone(),
        }
    }
}

impl CaptureGate for MetadataPriorityGate {
    fn work_control(&self, row: &domain::LedgerRow) -> marker::WorkControl {
        let owner = {
            let store = self.store.lock();
            store.get_media(row.media_id).ok().flatten().and_then(|media|
                store.library_for_path_strict(std::path::Path::new(&row.path), media.kind).ok().flatten())
        };
        owner.and_then(|owner| self.registry.as_ref().and_then(|registry| registry.active_control(&owner.id)))
            .map(|control| control.signal().clone()).unwrap_or_default()
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
