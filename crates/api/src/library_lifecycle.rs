//! Library-scoped work leases. Registry -> Store is the only permitted lock order.
use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Notify;

#[derive(Default)]
pub struct LibraryWorkRegistry {
    entries: Mutex<HashMap<String, Entry>>,
    changed: Notify,
}

#[derive(Default)]
struct Entry {
    cancelled: bool,
    controls: HashMap<String, library::WorkControl>,
}

pub struct LibraryWorkLease {
    registry: Arc<LibraryWorkRegistry>,
    library_id: String,
    lease_id: String,
    control: library::WorkControl,
}

impl LibraryWorkLease {
    pub fn control(&self) -> &library::WorkControl { &self.control }
}

impl Drop for LibraryWorkLease {
    fn drop(&mut self) {
        if let Some(entry) = self.registry.entries.lock().get_mut(&self.library_id) {
            entry.controls.remove(&self.lease_id);
        }
        self.registry.changed.notify_waiters();
    }
}

impl LibraryWorkRegistry {
    pub fn acquire(self: &Arc<Self>, store: &Mutex<store::Store>, library_id: &str)
        -> Result<LibraryWorkLease, String>
    {
        let mut entries = self.entries.lock();
        let entry = entries.entry(library_id.to_string()).or_default();
        if entry.cancelled || !store.lock().library_accepts_work(library_id).map_err(|e| e.to_string())? {
            return Err("Library 正在删除或已不存在".into());
        }
        let lease_id = uuid::Uuid::new_v4().to_string();
        let control = library::WorkControl::default();
        entry.controls.insert(lease_id.clone(), control.clone());
        Ok(LibraryWorkLease { registry: self.clone(), library_id: library_id.into(), lease_id, control })
    }

    pub fn freeze(&self, store: &Mutex<store::Store>, library_id: &str)
        -> Result<store::LibraryRemoval, store::StoreError>
    {
        let mut entries = self.entries.lock();
        let operation = store.lock().begin_library_removal(library_id)?;
        let entry = entries.entry(library_id.into()).or_default();
        entry.cancelled = true;
        for control in entry.controls.values() { control.cancel(); }
        Ok(operation)
    }

    pub fn cancel(&self, library_id: &str) {
        let mut entries = self.entries.lock();
        let entry = entries.entry(library_id.into()).or_default();
        entry.cancelled = true;
        for control in entry.controls.values() { control.cancel(); }
    }

    pub async fn wait_idle(&self, library_id: &str) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let idle = self.entries.lock().get(library_id).is_none_or(|entry| entry.controls.is_empty());
            if idle { return; }
            notified.await;
        }
    }
}
