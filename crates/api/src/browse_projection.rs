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
//!
//! 终态发布是显式协议：leader 先把终态写入 inflight 条目（可重放），再广播唤醒
//! 等待者，最后移除条目。follower 在 map 锁内先订阅再读终态，因此「发布之后、
//! 条目移除之前」进入的 follower 也能拿到真实 `Ready`/`Failed`/`StaleScope`，
//! 而不是把错过的广播误报成 `Cancelled`/`Timeout`。

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

/// 测试 seam：终态发布之后、inflight 条目移除之前按 Library 执行一次。
///
/// 生产不设置（`None`），因此生产路径只多一次 `Option` 判断；测试注入它来把
/// follower 确定性地放进「终态已发布、条目尚未移除」的窗口，验证 follower 重放
/// 真实终态而不是伪造 `Cancelled`/`Timeout`。
pub type PublishBarrier = Arc<dyn Fn(&str) + Send + Sync>;

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

/// 一次构建的终态。除广播唤醒等待者外，终态还写入 [`Inflight`] 供「发布之后、
/// 条目移除之前」进入的 follower 重放，保证 leader/follower 对同一结果一致，
/// 而不是各自 fallback 或各自超时。
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

/// 终态 → `ensure` 返回值：只有 `Ready` 是成功，其余映射为对应错误。
fn outcome_result(outcome: BuildOutcome) -> Result<ProjectionSnapshot, ProjectionError> {
    match outcome {
        BuildOutcome::Ready(snapshot) => Ok(snapshot),
        other => Err(other.into_error()),
    }
}

type InflightMap = HashMap<String, Arc<Inflight>>;

/// One in-progress build for a Library. Followers subscribe to `tx` and observe
/// the same outcome the blocking build publishes.
///
/// 终态协议（顺序不可交换）：
/// - 发布方 [`Inflight::publish`] 先写 `outcome`，再 `send` 唤醒等待者；
/// - follower [`Inflight::join`] 先 `subscribe`，再读 `outcome`。
///
/// 于是 follower 在 map 锁内拿到条目后终态必定可取得：读到 `Some` 直接重放
/// （即使 leader 已经 `send` 过、receiver 错过消息），读到 `None` 说明发布尚未
/// 发生、随后的 `send` 必达已订阅的 receiver。条目移除发生在 `publish` 之后，
/// 因此不存在「拿到条目却拿不到终态」的窗口。
struct Inflight {
    tx: broadcast::Sender<BuildOutcome>,
    outcome: Mutex<Option<BuildOutcome>>,
}

impl Inflight {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(1);
        Self {
            tx,
            outcome: Mutex::new(None),
        }
    }

    /// 发布终态：先落可重放状态，再唤醒等待者。
    fn publish(&self, outcome: BuildOutcome) {
        *self.outcome.lock() = Some(outcome.clone());
        let _ = self.tx.send(outcome);
    }

    /// 加入一次在飞构建：先订阅，再读已发布终态（重放值）。
    fn join(&self) -> (broadcast::Receiver<BuildOutcome>, Option<BuildOutcome>) {
        let rx = self.tx.subscribe();
        let replay = self.outcome.lock().clone();
        (rx, replay)
    }
}

/// `ensure` 在 inflight map 锁内做出的选择。
enum Join {
    /// 本请求成为 leader：需要真正构建并发布。
    Leader(Arc<Inflight>),
    /// 已有 leader：携带订阅与 join 瞬间已发布的终态（若有）。
    Follower {
        rx: broadcast::Receiver<BuildOutcome>,
        replay: Option<BuildOutcome>,
    },
}

/// 终态发布守卫：由 blocking 构建闭包持有，在闭包真正结束时（正常返回或
/// panic）**先发布终态、再移除 inflight 条目**。
///
/// 闭包通过 [`InflightFinish::complete`] 交出真实终态；若闭包未及交出就结束
/// （panic / 运行时关闭），补一个 `Failed` 终态。这样「条目存在 ⇒ 终态可取得」，
/// 不存在「subscriber 拿到条目却拿不到终态」的窗口。
///
/// 条目移除发生在 `publish` 之后且仅当条目仍是本次构建（`Arc::ptr_eq`），因此
/// 超时/取消的请求不会让条目提前消失，重试仍加入同一次在飞构建。
struct InflightFinish {
    inflight: Arc<Mutex<InflightMap>>,
    library_id: String,
    entry: Arc<Inflight>,
    barrier: Option<PublishBarrier>,
    outcome: Option<BuildOutcome>,
}

impl InflightFinish {
    fn complete(&mut self, outcome: BuildOutcome) {
        self.outcome = Some(outcome);
    }
}

impl Drop for InflightFinish {
    fn drop(&mut self) {
        let outcome = self.outcome.take().unwrap_or_else(|| {
            tracing::error!(
                library_id = %self.library_id,
                "投影构建未发布终态（blocking 闭包 panic？），按 Failed 收尾"
            );
            BuildOutcome::Failed("projection build ended without an outcome".into())
        });
        self.entry.publish(outcome);
        if let Some(barrier) = &self.barrier {
            barrier(&self.library_id);
        }
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
    /// 测试 seam：终态发布后、条目移除前的屏障（生产为 `None`）。
    publish_barrier: Option<PublishBarrier>,
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
            publish_barrier: None,
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

    /// 注入终态发布屏障（测试 seam）：在终态发布之后、inflight 条目移除之前
    /// 按 Library 执行一次，用来确定性地复现「发布后、移除前进入的 follower」。
    /// 生产不调用，行为不变。
    pub fn with_publish_barrier(mut self, barrier: PublishBarrier) -> Self {
        self.publish_barrier = Some(barrier);
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
    /// coordinates a single-flight rebuild. Followers join the leader's inflight
    /// entry under the same deadline: a terminal outcome already published is
    /// replayed, otherwise the follower waits for the leader's broadcast. Builds
    /// share a bounded capacity.
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
        // (see `InflightFinish`), so it stays after this wait ends and retries
        // join the running build instead of starting a second one.
        let join = {
            let mut map = self.inflight.lock();
            match map.get(&library_id) {
                // 先订阅再读终态：已发布则重放，未发布则后续 send 必达。
                Some(entry) => {
                    let (rx, replay) = entry.join();
                    Join::Follower { rx, replay }
                }
                None => {
                    let entry = Arc::new(Inflight::new());
                    map.insert(library_id.clone(), entry.clone());
                    Join::Leader(entry)
                }
            }
        };

        let entry = match join {
            Join::Leader(entry) => entry,
            Join::Follower { rx, replay } => {
                // leader 可能在我们 freshness 检查与 join 之间已发布终态：重放它，
                // 而不是在广播上错过消息后伪造 Cancelled/Timeout。
                if let Some(outcome) = replay {
                    return outcome_result(outcome);
                }
                if let Some(snapshot) = self.fresh_snapshot(&state, &library_id).await? {
                    return Ok(snapshot);
                }
                let mut rx = rx;
                return match tokio::time::timeout(self.deadline, rx.recv()).await {
                    Ok(Ok(outcome)) => outcome_result(outcome),
                    // 守卫保证先发布再移除，因此下面两个分支只是不可达兜底。
                    Ok(Err(broadcast::error::RecvError::Closed)) => Err(ProjectionError::Cancelled),
                    Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                        Err(ProjectionError::Cancelled)
                    }
                    Err(_) => Err(ProjectionError::Timeout),
                };
            }
        };

        let dirty_version_before = self.dirty_libraries.lock().get(&library_id).copied();
        let outcome = self
            .build_and_publish(state, library_id.clone(), entry, dirty_version_before)
            .await;

        outcome_result(outcome)
    }

    /// 构建 + 发布 + 回读，全部在共享构建容量与 deadline 内、blocking 线程上完成。
    ///
    /// inflight 条目由 blocking 闭包持有的 [`InflightFinish`] 在构建真正结束时
    /// **先发布终态、再移除**；请求侧 deadline 只结束本次等待，超时不会让同一
    /// Library 再起第二次构建。dirty 清理同样在闭包里按 `dirty_version_before`
    /// CAS，构建期间的新脏标记不会被这次发布吞掉。
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
        let inflight = self.inflight.clone();
        let barrier = self.publish_barrier.clone();
        let job_library = library_id.clone();
        let job_entry = entry.clone();
        let result = budget
            .run(deadline, move || {
                // 守卫在闭包内创建：Busy（未取得 permit、闭包未运行）时不存在守卫，
                // 由下面的 Busy 分支发布终态并移除条目。
                let mut finish = InflightFinish {
                    inflight,
                    library_id: job_library.clone(),
                    entry: job_entry,
                    barrier,
                    outcome: None,
                };
                let outcome = publish_blocking(&state, &job_library);
                if matches!(outcome, BuildOutcome::Ready(_)) {
                    let mut dirty = dirty_libraries.lock();
                    if dirty.get(&job_library).copied() == dirty_version_before {
                        dirty.remove(&job_library);
                    }
                }
                finish.complete(outcome.clone());
                outcome
            })
            .await;
        match result {
            Ok(outcome) => outcome,
            // 未取得 permit：没有任何构建在跑，通知 follower 并交回条目让重试重建。
            Err(BlockingError::Busy) => {
                self.publish_terminal(&library_id, &entry, BuildOutcome::Busy);
                BuildOutcome::Busy
            }
            // 等待结束，但 blocking 构建仍在跑；条目由守卫在真正结束时发布并移除。
            Err(BlockingError::Timeout) => BuildOutcome::Timeout,
            // 闭包 panic：守卫已按 Failed 收尾并移除条目，这里只回报 leader。
            Err(BlockingError::Worker(message)) => {
                tracing::error!(library_id = %library_id, %message, "投影构建 blocking worker 失败");
                BuildOutcome::Failed(message)
            }
        }
    }

    /// 终态发布协议（显式顺序，不可交换）：写入终态 → 唤醒 follower →
    /// 测试屏障（生产为 `None`）→ 移除条目。移除前的任何进入者都能取得终态。
    fn publish_terminal(&self, library_id: &str, entry: &Arc<Inflight>, outcome: BuildOutcome) {
        entry.publish(outcome);
        if let Some(barrier) = &self.publish_barrier {
            barrier(library_id);
        }
        self.remove_inflight(library_id, entry);
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
