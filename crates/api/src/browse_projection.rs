//! Asynchronous ProjectionService orchestrating single-flight builds and periodic refresh.
//!
//! 浏览读路径通过 [`ProjectionService::ensure`] 取目标 Library 的 ready 投影：
//! - **公共事实** revision（app/library/catalog）或 scope 指纹变化 → 视为 dirty，
//!   单飞重建；`subscribe`（用户 `playback_units`）不参与公共投影新鲜度判定，
//!   用户观看状态每请求按目标 Media 批量实时读取；
//! - `mark_dirty` / `mark_all_dirty` 由生产写路径（ledger / Media / catalog / 刮削 /
//!   图片 / STRM）调用，覆盖不递增 revision 的文件事实变化；
//! - `start_periodic_refresh` 每分钟刷新**已访问**的 Library，使外部 NFO/图片修改
//!   在 ≤60 秒 + 构建排队延迟内可见。
//!
//! 新鲜度检查、构建、发布与回读全部在 blocking 预算里执行，不在 Tokio worker 上
//! 持 Store 锁做 DB 读写；构建容量有界（不足 503）、leader 与 follower 共用同一
//! deadline（超时 504）。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use store::{ProjectionSnapshot, ReadRevisions};
use tokio::sync::broadcast;

use crate::blocking_budget::{BlockingBudget, BlockingError};
use crate::management::ApiState;

pub mod builder;
pub mod facts;

/// 投影构建与 follower 等待共用的等待预算。
pub(crate) const PROJECTION_DEADLINE: Duration = Duration::from_secs(30);
/// 共享构建容量：不同 Library 的 leader 并发重建不得超过该数，超出即 503。
const PROJECTION_BUILD_CAPACITY: usize = 2;

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

/// 一次构建的终态。广播给同一 Library 的 follower，保证 leader/follower 对
/// Busy/Timeout/Cancelled 看到同一结果，而不是各自 fallback 或各自超时。
#[derive(Clone, Debug)]
enum BuildOutcome {
    Ready(ProjectionSnapshot),
    Busy,
    Timeout,
    Cancelled,
    StaleScope,
    Failed(String),
}

impl BuildOutcome {
    fn into_error(self) -> ProjectionError {
        match self {
            // 调用方只对非 Ready 取错误；Ready 分支在 `ensure` 里先被消费。
            BuildOutcome::Ready(_) => ProjectionError::Failed("projection ready".into()),
            BuildOutcome::Busy => ProjectionError::Busy,
            BuildOutcome::Timeout => ProjectionError::Timeout,
            BuildOutcome::Cancelled => ProjectionError::Cancelled,
            BuildOutcome::StaleScope => ProjectionError::StaleScope,
            BuildOutcome::Failed(message) => ProjectionError::Failed(message),
        }
    }

    fn from_error(error: ProjectionError) -> Self {
        match error {
            ProjectionError::Busy => BuildOutcome::Busy,
            ProjectionError::Timeout => BuildOutcome::Timeout,
            ProjectionError::Cancelled => BuildOutcome::Cancelled,
            ProjectionError::StaleScope => BuildOutcome::StaleScope,
            ProjectionError::Failed(message) => BuildOutcome::Failed(message),
        }
    }
}

type InflightMap = HashMap<String, Arc<Inflight>>;

/// One in-progress build for a Library. Followers subscribe to `tx` and observe
/// the same outcome the blocking build publishes.
struct Inflight {
    tx: broadcast::Sender<BuildOutcome>,
}

/// Held by the **blocking build closure** and dropped when that closure actually
/// ends. The inflight entry therefore outlives a request's wait: a request
/// timeout only ends the wait, while retries for the same Library join the
/// running build instead of starting a second one. `entry` identity is checked
/// so a stale guard cannot evict a newer build's entry.
struct InflightRemoval {
    inflight: Arc<Mutex<InflightMap>>,
    library_id: String,
    entry: Arc<Inflight>,
}

impl Drop for InflightRemoval {
    fn drop(&mut self) {
        let mut map = self.inflight.lock();
        if map
            .get(&self.library_id)
            .is_some_and(|current| Arc::ptr_eq(current, &self.entry))
        {
            map.remove(&self.library_id);
        }
    }
}

pub struct ProjectionService {
    dirty_libraries: Arc<Mutex<HashMap<String, u64>>>,
    /// 至少被浏览过一次的 Library；周期刷新只覆盖这些库。
    visited: Mutex<HashSet<String>>,
    /// 每个 Library 至多一个进行中的构建；条目由 blocking 闭包在结束时移除。
    inflight: Arc<Mutex<InflightMap>>,
    /// 有界构建容量：permit 由 blocking 构建闭包持有到真正结束。
    build_budget: Arc<BlockingBudget>,
    /// leader 构建与 follower 等待共用的 deadline。
    deadline: Duration,
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
            dirty_libraries: Arc::new(Mutex::new(HashMap::new())),
            visited: Mutex::new(HashSet::new()),
            inflight: Arc::new(Mutex::new(HashMap::new())),
            build_budget: Arc::new(BlockingBudget::new(PROJECTION_BUILD_CAPACITY)),
            deadline: PROJECTION_DEADLINE,
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

    /// 替换共享构建容量（测试注入 1 以观察容量不足 503）。
    pub fn with_build_capacity(mut self, capacity: usize) -> Self {
        self.build_budget = Arc::new(BlockingBudget::new(capacity));
        self
    }

    /// 替换 leader 构建 / follower 等待 deadline（测试注入短预算以观察 504）。
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
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
    /// coordinates a single-flight rebuild. Followers wait for the leader's build
    /// under the same deadline; builds share a bounded capacity.
    pub async fn ensure(
        &self,
        state: ApiState,
        library_id: String,
    ) -> Result<ProjectionSnapshot, ProjectionError> {
        if self.disabled {
            return Err(ProjectionError::Failed("browse projection disabled".into()));
        }
        self.visited.lock().insert(library_id.clone());

        if let Some(snapshot) = self.fresh_snapshot(&state, &library_id).await? {
            return Ok(snapshot);
        }

        // Single-flight coordination: the entry is owned by the blocking build
        // (see `InflightRemoval`), so it stays after this wait ends and retries
        // join the running build instead of starting a second one.
        let (rx, leader) = {
            let mut map = self.inflight.lock();
            if let Some(entry) = map.get(&library_id) {
                (entry.tx.subscribe(), None)
            } else {
                let (tx, rx) = broadcast::channel(1);
                let entry = Arc::new(Inflight { tx });
                map.insert(library_id.clone(), entry.clone());
                (rx, Some(entry))
            }
        };

        let Some(entry) = leader else {
            // The leader may have published between the freshness check and our
            // subscribe; re-check before paying the deadline wait.
            if let Some(snapshot) = self.fresh_snapshot(&state, &library_id).await? {
                return Ok(snapshot);
            }
            let mut rx = rx;
            return match tokio::time::timeout(self.deadline, rx.recv()).await {
                Ok(Ok(BuildOutcome::Ready(snap))) => Ok(snap),
                Ok(Ok(outcome)) => Err(outcome.into_error()),
                Ok(Err(_)) => Err(ProjectionError::Cancelled),
                Err(_) => Err(ProjectionError::Timeout),
            };
        };

        let dirty_version_before = self.dirty_libraries.lock().get(&library_id).copied();
        let outcome = self
            .build_and_publish(state, library_id.clone(), entry, dirty_version_before)
            .await;

        match outcome {
            BuildOutcome::Ready(snapshot) => Ok(snapshot),
            other => Err(other.into_error()),
        }
    }

    /// 构建 + 发布 + 回读，全部在共享构建容量与 deadline 内、blocking 线程上完成。
    ///
    /// inflight 条目由 blocking 闭包持有的 [`InflightRemoval`] 在构建真正结束时
    /// 移除；请求侧 deadline 只结束本次等待，超时不会让同一 Library 再起第二次
    /// 构建。dirty 清理同样在闭包里按 `dirty_version_before` CAS，构建期间的新
    /// 脏标记不会被这次发布吞掉。
    async fn build_and_publish(
        &self,
        state: ApiState,
        library_id: String,
        entry: Arc<Inflight>,
        dirty_version_before: Option<u64>,
    ) -> BuildOutcome {
        let budget = self.build_budget.clone();
        let deadline = self.deadline;
        let dirty_libraries = self.dirty_libraries.clone();
        let job_library = library_id.clone();
        let job_entry = entry.clone();
        let removal = InflightRemoval {
            inflight: self.inflight.clone(),
            library_id: library_id.clone(),
            entry: entry.clone(),
        };
        let result = budget
            .run(deadline, move || {
                let outcome = publish_blocking(&state, &job_library);
                if matches!(outcome, BuildOutcome::Ready(_)) {
                    let mut dirty = dirty_libraries.lock();
                    if dirty.get(&job_library).copied() == dirty_version_before {
                        dirty.remove(&job_library);
                    }
                }
                let _ = job_entry.tx.send(outcome.clone());
                drop(removal);
                outcome
            })
            .await;
        match result {
            Ok(outcome) => outcome,
            // 未取得 permit：没有任何构建在跑，通知 follower 并交回条目让重试重建。
            Err(BlockingError::Busy) => {
                let _ = entry.tx.send(BuildOutcome::Busy);
                self.remove_inflight(&library_id, &entry);
                BuildOutcome::Busy
            }
            // 等待结束，但 blocking 构建仍在跑；条目由闭包在真正结束时移除。
            Err(BlockingError::Timeout) => BuildOutcome::Timeout,
            Err(BlockingError::Worker(message)) => {
                let outcome = BuildOutcome::Failed(message);
                let _ = entry.tx.send(outcome.clone());
                self.remove_inflight(&library_id, &entry);
                outcome
            }
        }
    }

    /// 仅在条目仍是本次构建时移除，避免误删更新的构建。
    fn remove_inflight(&self, library_id: &str, entry: &Arc<Inflight>) {
        let mut map = self.inflight.lock();
        if map
            .get(library_id)
            .is_some_and(|current| Arc::ptr_eq(current, entry))
        {
            map.remove(library_id);
        }
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

    /// 新鲜度检查的 DB 读也在 blocking 预算里执行，不在 async worker 上持 Store 锁。
    async fn fresh_snapshot(
        &self,
        state: &ApiState,
        library_id: &str,
    ) -> Result<Option<ProjectionSnapshot>, ProjectionError> {
        if self.dirty_libraries.lock().contains_key(library_id) {
            return Ok(None);
        }
        let budget = state.http_work.clone();
        let state = state.clone();
        let library_id = library_id.to_string();
        match budget
            .run(
                crate::http::library::selection::BROWSE_DEADLINE,
                move || fresh_snapshot_inner(&state, &library_id),
            )
            .await
        {
            Ok(result) => result,
            Err(BlockingError::Busy) => Err(ProjectionError::Busy),
            Err(BlockingError::Timeout) => Err(ProjectionError::Timeout),
            Err(BlockingError::Worker(message)) => Err(ProjectionError::Failed(message)),
        }
    }
}

/// 锁内读当前 revision / scope 指纹 / 投影，公共事实全部一致才返回 ready 快照。
fn fresh_snapshot_inner(
    state: &ApiState,
    library_id: &str,
) -> Result<Option<ProjectionSnapshot>, ProjectionError> {
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
        snap.revisions.public_matches(&current.revisions)
            && Some(snap.scope_signature.as_str()) == current.scope_signature.as_deref()
    }))
}

/// blocking 线程上的构建 → 发布 → 回读；Store 锁只包住短 DB 读写。
fn publish_blocking(state: &ApiState, library_id: &str) -> BuildOutcome {
    let build = match builder::build(state, library_id) {
        Ok(build) => build,
        Err(error) => return BuildOutcome::from_error(error),
    };
    let store = state.store.lock();
    match store.publish_browse_projection(&build) {
        Ok(true) => match store.browse_projection(library_id) {
            Ok(Some(snapshot)) => BuildOutcome::Ready(snapshot),
            Ok(None) => BuildOutcome::Failed("Projection missing after publish".into()),
            Err(error) => BuildOutcome::Failed(error.to_string()),
        },
        Ok(false) => BuildOutcome::StaleScope,
        Err(error) => BuildOutcome::Failed(error.to_string()),
    }
}

impl Default for ProjectionService {
    fn default() -> Self {
        Self::new()
    }
}
