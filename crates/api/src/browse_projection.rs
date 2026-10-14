//! Asynchronous ProjectionService orchestrating single-flight builds and periodic refresh.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use store::ProjectionSnapshot;
use tokio::sync::broadcast;

use crate::management::ApiState;

pub mod builder;

#[derive(Debug)]
pub enum ProjectionError {
    Busy,
    Timeout,
    Cancelled,
    StaleScope,
    Failed(String),
}

impl From<String> for ProjectionError {
    fn from(s: String) -> Self {
        Self::Failed(s)
    }
}

struct InflightLeaderGuard<'a> {
    inflight: &'a Mutex<HashMap<String, broadcast::Sender<Result<ProjectionSnapshot, String>>>>,
    library_id: String,
    tx: Option<broadcast::Sender<Result<ProjectionSnapshot, String>>>,
}

impl<'a> Drop for InflightLeaderGuard<'a> {
    fn drop(&mut self) {
        let mut map = self.inflight.lock();
        map.remove(&self.library_id);
    }
}

pub struct ProjectionService {
    dirty_libraries: Mutex<HashMap<String, u64>>,
    inflight: Mutex<HashMap<String, broadcast::Sender<Result<ProjectionSnapshot, String>>>>,
}

impl ProjectionService {
    pub fn new() -> Self {
        Self {
            dirty_libraries: Mutex::new(HashMap::new()),
            inflight: Mutex::new(HashMap::new()),
        }
    }

    /// Mark a library projection as dirty.
    pub fn mark_dirty(&self, library_id: &str) {
        let mut map = self.dirty_libraries.lock();
        let counter = map.entry(library_id.to_string()).or_insert(0);
        *counter += 1;
    }

    /// Ensure a valid projection snapshot is available for `library_id`.
    ///
    /// If snapshot is missing or dirty, coordinates a single-flight rebuild.
    pub async fn ensure(
        &self,
        state: ApiState,
        library_id: String,
    ) -> Result<ProjectionSnapshot, ProjectionError> {
        let cur_revs = {
            let store = state.store.lock();
            store.read_revisions().map_err(|e| ProjectionError::Failed(e.to_string()))?
        };

        // Fast path: check if existing projection in store is ready and not dirty
        let is_dirty = self.dirty_libraries.lock().contains_key(&library_id);
        if !is_dirty {
            let snap_opt = {
                let store = state.store.lock();
                store.browse_projection(&library_id).map_err(|e| ProjectionError::Failed(e.to_string()))?
            };
            if let Some(snap) = snap_opt {
                if snap.revisions == cur_revs {
                    return Ok(snap);
                }
            }
        }

        // Single-flight coordination
        let (rx, leader_guard) = {
            let mut map = self.inflight.lock();
            if let Some(tx) = map.get(&library_id) {
                (tx.subscribe(), None)
            } else {
                let (tx, rx) = broadcast::channel(1);
                map.insert(library_id.clone(), tx.clone());
                let guard = InflightLeaderGuard {
                    inflight: &self.inflight,
                    library_id: library_id.clone(),
                    tx: Some(tx),
                };
                (rx, Some(guard))
            }
        };

        if leader_guard.is_none() {
            let mut rx = rx;
            match tokio::time::timeout(Duration::from_secs(30), rx.recv()).await {
                Ok(Ok(Ok(snap))) => return Ok(snap),
                Ok(Ok(Err(err))) => return Err(ProjectionError::Failed(err)),
                Ok(Err(_)) => return Err(ProjectionError::Cancelled),
                Err(_) => return Err(ProjectionError::Timeout),
            }
        }

        let mut leader_guard = leader_guard.unwrap();
        let dirty_version_before = self.dirty_libraries.lock().get(&library_id).copied();

        // Leader executes build with RAII inflight guard
        let state_clone = state.clone();
        let lib_id = library_id.clone();
        let build_res = tokio::task::spawn_blocking(move || {
            builder::build(&state_clone, &lib_id)
        })
        .await
        .map_err(|e| ProjectionError::Failed(e.to_string()))?;

        let res = match build_res {
            Ok(build) => {
                let store = state.store.lock();
                let pub_res = store.publish_browse_projection(&build);
                match pub_res {
                    Ok(true) => {
                        let mut dirty_map = self.dirty_libraries.lock();
                        if dirty_map.get(&library_id).copied() == dirty_version_before {
                            dirty_map.remove(&library_id);
                        }
                        store
                            .browse_projection(&library_id)
                            .map_err(|e| ProjectionError::Failed(e.to_string()))?
                            .ok_or_else(|| ProjectionError::Failed("Projection missing after publish".into()))
                    }
                    Ok(false) => Err(ProjectionError::StaleScope),
                    Err(e) => Err(ProjectionError::Failed(e.to_string())),
                }
            }
            Err(e) => Err(e),
        };

        // Broadcast to followers and drop leader guard
        let broadcast_payload = res.as_ref().map(|s| s.clone()).map_err(|e| format!("{:?}", e));
        if let Some(tx) = leader_guard.tx.take() {
            let _ = tx.send(broadcast_payload);
        }
        drop(leader_guard);

        res
    }

    /// Refresh dirty libraries in a background loop every 60 seconds.
    pub fn start_periodic_refresh(self: Arc<Self>, state: ApiState) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                let dirty: Vec<String> = self
                    .dirty_libraries
                    .lock()
                    .keys()
                    .cloned()
                    .collect();
                for lib_id in dirty {
                    let _ = self.ensure(state.clone(), lib_id).await;
                }
            }
        });
    }
}
