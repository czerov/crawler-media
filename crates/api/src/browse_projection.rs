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

type InflightMap = HashMap<String, broadcast::Sender<BuildOutcome>>;

struct InflightLeaderGuard<'a> {
    inflight: &'a Mutex<InflightMap>,
    library_id: String,
    tx: Option<broadcast::Sender<BuildOutcome>>,
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
            dirty_libraries: Mutex::new(HashMap::new()),
            visited: Mutex::new(HashSet::new()),
            inflight: Mutex::new(HashMap::new()),
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
        }

        let mut leader_guard = leader_guard.unwrap();
        let dirty_version_before = self.dirty_libraries.lock().get(&library_id).copied();
        let outcome = self.build_and_publish(state, library_id.clone()).await;

        if matches!(outcome, BuildOutcome::Ready(_)) {
            let mut dirty_map = self.dirty_libraries.lock();
            if dirty_map.get(&library_id).copied() == dirty_version_before {
                dirty_map.remove(&library_id);
            }
        }

        if let Some(tx) = leader_guard.tx.take() {
            let _ = tx.send(outcome.clone());
        }
        drop(leader_guard);

        match outcome {
            BuildOutcome::Ready(snapshot) => Ok(snapshot),
            other => Err(other.into_error()),
        }
    }

    /// 构建 + 发布 + 回读，全部在共享构建容量与 deadline 内、blocking 线程上完成。
    async fn build_and_publish(&self, state: ApiState, library_id: String) -> BuildOutcome {
        let budget = self.build_budget.clone();
        let deadline = self.deadline;
        match budget
            .run(deadline, move || publish_blocking(&state, &library_id))
            .await
        {
            Ok(outcome) => outcome,
            Err(BlockingError::Busy) => BuildOutcome::Busy,
            Err(BlockingError::Timeout) => BuildOutcome::Timeout,
            Err(BlockingError::Worker(message)) => BuildOutcome::Failed(message),
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
