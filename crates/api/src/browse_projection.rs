//! Asynchronous ProjectionService orchestrating single-flight builds and periodic refresh.
//!
//! 浏览读路径通过 [`ProjectionService::ensure`] 取目标 Library 的 ready 投影：
//! - revision（app/library/catalog/subscribe）或 scope 指纹变化 → 视为 dirty，单飞重建；
//! - `mark_dirty` / `mark_all_dirty` 由生产写路径（ledger / Media / catalog / 刮削 /
//!   图片 / STRM）调用，覆盖不递增 revision 的文件事实变化；
//! - `start_periodic_refresh` 每分钟刷新**已访问**的 Library，使外部 NFO/图片修改
//!   在 ≤60 秒 + 构建排队延迟内可见。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use store::{ProjectionSnapshot, ReadRevisions};
use tokio::sync::broadcast;

use crate::management::ApiState;

pub mod builder;
pub mod facts;

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

type InflightMap = HashMap<String, broadcast::Sender<Result<ProjectionSnapshot, String>>>;

struct InflightLeaderGuard<'a> {
    inflight: &'a Mutex<InflightMap>,
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
    /// 至少被浏览过一次的 Library；周期刷新只覆盖这些库。
    visited: Mutex<HashSet<String>>,
    inflight: Mutex<InflightMap>,
    /// 运维/测试开关：为真时 `ensure` 恒失败，读路径走旧的全量路径。
    disabled: bool,
}

/// 锁内取得的当前状态：revision、scope 指纹与已发布投影。
struct Current {
    revisions: ReadRevisions,
    scope_signature: Option<String>,
    snapshot: Option<ProjectionSnapshot>,
}

impl ProjectionService {
    pub fn new() -> Self {
        Self {
            dirty_libraries: Mutex::new(HashMap::new()),
            visited: Mutex::new(HashSet::new()),
            inflight: Mutex::new(HashMap::new()),
            disabled: false,
        }
    }

    /// 关闭投影：`ensure` 恒失败，浏览读路径回退到旧实现（回退开关）。
    pub fn disabled() -> Self {
        Self {
            disabled: true,
            ..Self::new()
        }
    }

    /// Mark a library projection as dirty.
    pub fn mark_dirty(&self, library_id: &str) {
        let mut map = self.dirty_libraries.lock();
        let counter = map.entry(library_id.to_string()).or_insert(0);
        *counter += 1;
    }

    /// Mark every visited library dirty (Media/catalog changes affect all libraries).
    pub fn mark_all_dirty(&self) {
        let visited: Vec<String> = self.visited.lock().iter().cloned().collect();
        for library_id in visited {
            self.mark_dirty(&library_id);
        }
    }

    /// Whether the given library currently has a pending dirty flag.
    pub fn is_dirty(&self, library_id: &str) -> bool {
        self.dirty_libraries.lock().contains_key(library_id)
    }

    /// Ensure a valid projection snapshot is available for `library_id`.
    ///
    /// If the snapshot is missing, dirty, or built for a different revision/scope,
    /// coordinates a single-flight rebuild. Followers wait for the leader's build.
    pub async fn ensure(
        &self,
        state: ApiState,
        library_id: String,
    ) -> Result<ProjectionSnapshot, ProjectionError> {
        if self.disabled {
            return Err(ProjectionError::Failed("browse projection disabled".into()));
        }
        self.visited.lock().insert(library_id.clone());

        if let Some(snapshot) = self.fresh_snapshot(&state, &library_id)? {
            return Ok(snapshot);
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
            // The leader may have published between the freshness check and our
            // subscribe; re-check before paying the 30s wait.
            if let Some(snapshot) = self.fresh_snapshot(&state, &library_id)? {
                return Ok(snapshot);
            }
            let mut rx = rx;
            return match tokio::time::timeout(Duration::from_secs(30), rx.recv()).await {
                Ok(Ok(Ok(snap))) => Ok(snap),
                Ok(Ok(Err(err))) => Err(ProjectionError::Failed(err)),
                Ok(Err(_)) => Err(ProjectionError::Cancelled),
                Err(_) => Err(ProjectionError::Timeout),
            };
        }

        let mut leader_guard = leader_guard.unwrap();
        let dirty_version_before = self.dirty_libraries.lock().get(&library_id).copied();
        let state_clone = state.clone();
        let lib_id = library_id.clone();
        let build_res = tokio::task::spawn_blocking(move || builder::build(&state_clone, &lib_id))
            .await
            .map_err(|e| ProjectionError::Failed(e.to_string()))?;

        let res = match build_res {
            Ok(build) => {
                let store = state.store.lock();
                match store.publish_browse_projection(&build) {
                    Ok(true) => {
                        let mut dirty_map = self.dirty_libraries.lock();
                        if dirty_map.get(&library_id).copied() == dirty_version_before {
                            dirty_map.remove(&library_id);
                        }
                        drop(dirty_map);
                        store
                            .browse_projection(&library_id)
                            .map_err(|e| ProjectionError::Failed(e.to_string()))?
                            .ok_or_else(|| {
                                ProjectionError::Failed("Projection missing after publish".into())
                            })
                    }
                    Ok(false) => Err(ProjectionError::StaleScope),
                    Err(e) => Err(ProjectionError::Failed(e.to_string())),
                }
            }
            Err(e) => Err(e),
        };

        let broadcast_payload = res.as_ref().map(|s| s.clone()).map_err(|e| format!("{e:?}"));
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
                // 已访问库的文件事实可能被外部修改（NFO/图片/STRM），revision 不会动，
                // 因此周期标脏：稳态请求仍读投影，刷新在后台有界顺序执行。
                self.mark_all_dirty();
                let visited: Vec<String> = self.visited.lock().iter().cloned().collect();
                for lib_id in visited {
                    match self.ensure(state.clone(), lib_id.clone()).await {
                        Ok(_) => {}
                        // Library 已删除 / 不再接受工作：停止刷新，避免每分钟刷日志。
                        Err(ProjectionError::Cancelled) => {
                            self.visited.lock().remove(&lib_id);
                        }
                        Err(error) => {
                            tracing::warn!(library_id = %lib_id, ?error, "周期刷新浏览投影失败");
                        }
                    }
                }
            }
        });
    }

    /// 锁内读当前 revision / scope 指纹 / 投影，全部一致才返回 ready 快照。
    fn fresh_snapshot(
        &self,
        state: &ApiState,
        library_id: &str,
    ) -> Result<Option<ProjectionSnapshot>, ProjectionError> {
        if self.dirty_libraries.lock().contains_key(library_id) {
            return Ok(None);
        }
        let current = {
            let store = state.store.lock();
            let revisions = store
                .read_revisions()
                .map_err(|e| ProjectionError::Failed(e.to_string()))?;
            let scope_signature = store
                .get_library(library_id)
                .map_err(|e| ProjectionError::Failed(e.to_string()))?
                .map(|library| builder::scope_signature(&library));
            let snapshot = store
                .browse_projection(library_id)
                .map_err(|e| ProjectionError::Failed(e.to_string()))?;
            Current {
                revisions,
                scope_signature,
                snapshot,
            }
        };
        Ok(current.snapshot.filter(|snap| {
            snap.revisions == current.revisions
                && Some(snap.scope_signature.as_str()) == current.scope_signature.as_deref()
        }))
    }
}

impl Default for ProjectionService {
    fn default() -> Self {
        Self::new()
    }
}
