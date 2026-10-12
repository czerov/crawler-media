//! 共享 single-flight 字幕交付。
//!
//! key = (canonical ledger, source_version, index, wants_vtt)：同 key 只跑一个
//! blocking 提取任务，多个 waiter 通过 `tokio::sync::watch` 共享同一结果。
//! waiter 计数、加入与“最后一个 waiter 离开才取消”的判断都在同一个 map
//! 临界区内完成；取消只作用于本任务的独立 `WorkControl`，绝不取消
//! Library lease 的共享 control。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

use crate::management::ApiState;

const JOB_DEADLINE: Duration = Duration::from_secs(90);
const JOB_POLL: Duration = Duration::from_millis(25);
const CACHE_ROOT: &str = "crawler-media-subtitles";

#[derive(Clone, Debug)]
pub struct SubtitleRequest {
    pub ledger_id: domain::LedgerId,
    pub source_path: PathBuf,
    pub kind: domain::MediaKind,
    pub tracks: library::Tracks,
    pub index: u32,
    pub wants_vtt: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SubtitleServiceError {
    #[error("{0}")]
    Delivery(#[from] library::DeliveryError),
    #[error("subtitle operation timed out")]
    Timeout,
    #[error("subtitle service busy")]
    Busy,
    #[error("Library no longer available")]
    Cancelled,
    #[error("subtitle worker failed: {0}")]
    Worker(String),
}

// `library::DeliveryError` 没有 Clone（A6a 未加变体），这里按变体手工重建。
impl Clone for SubtitleServiceError {
    fn clone(&self) -> Self {
        match self {
            Self::Delivery(error) => Self::Delivery(clone_delivery(error)),
            Self::Timeout => Self::Timeout,
            Self::Busy => Self::Busy,
            Self::Cancelled => Self::Cancelled,
            Self::Worker(message) => Self::Worker(message.clone()),
        }
    }
}

fn clone_delivery(error: &library::DeliveryError) -> library::DeliveryError {
    use library::DeliveryError;
    match error {
        DeliveryError::TrackNotFound => DeliveryError::TrackNotFound,
        DeliveryError::FileMissing => DeliveryError::FileMissing,
        DeliveryError::Io(message) => DeliveryError::Io(message.clone()),
        DeliveryError::Extraction(message) => DeliveryError::Extraction(message.clone()),
    }
}

type JobResult = Result<library::SubtitlePayload, SubtitleServiceError>;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct SubtitleKey {
    ledger: domain::LedgerId,
    source_version: String,
    index: u32,
    wants_vtt: bool,
}

/// 一次在途提取。结果**保留在这里**，而不只是推给 channel：worker 可能在第一个
/// waiter 注册之前就结束，`watch::Sender::send` 在零 receiver 时会失败且不保存
/// 值，而 sender 仍被任务 Arc 保活 —— 晚到的 waiter 既读不到值也等不到关闭。
struct InFlight {
    outcome: Mutex<Option<JobResult>>,
    ready: Notify,
    control: library::WorkControl,
}

impl InFlight {
    fn new() -> Self {
        Self {
            outcome: Mutex::new(None),
            ready: Notify::new(),
            control: library::WorkControl::default(),
        }
    }

    /// 发布终态（幂等）并唤醒所有已注册的 waiter。
    fn finish(&self, outcome: JobResult) {
        {
            let mut slot = self.outcome.lock();
            if slot.is_some() {
                return;
            }
            *slot = Some(outcome);
        }
        self.ready.notify_waiters();
    }

    fn outcome(&self) -> Option<JobResult> {
        self.outcome.lock().clone()
    }
}

/// worker 任务即使 panic 也必须让 waiter 拿到明确结果，而不是永久等待。
struct FinishOnDrop(Arc<InFlight>);

impl Drop for FinishOnDrop {
    fn drop(&mut self) {
        self.0
            .finish(Err(SubtitleServiceError::Worker("subtitle worker dropped".into())));
    }
}

struct Entry {
    task: Arc<InFlight>,
    waiters: usize,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<SubtitleKey, Entry>,
}

pub struct SubtitleDelivery {
    runner: Arc<dyn library::SubtitleRunner>,
    permits: Arc<Semaphore>,
    inner: Arc<Mutex<Inner>>,
}

impl SubtitleDelivery {
    pub fn new(runner: Arc<dyn library::SubtitleRunner>, concurrency: usize) -> Self {
        Self {
            runner,
            permits: Arc::new(Semaphore::new(concurrency.max(1))),
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }

    pub async fn deliver(
        &self,
        state: &ApiState,
        request: SubtitleRequest,
    ) -> Result<library::SubtitlePayload, SubtitleServiceError> {
        let source_version = crate::fingerprint_job::current_source_version(&request.source_path);
        let key = SubtitleKey {
            ledger: request.ledger_id,
            source_version,
            index: request.index,
            wants_vtt: request.wants_vtt,
        };

        // 快路径：加入已有任务，不必消耗 permit。
        if let Some(waiter) = self.join_existing(&key) {
            return waiter.wait().await;
        }
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| SubtitleServiceError::Busy)?;
        // 选举必须是**同一个临界区内的原子决策**：查找已有任务、增加 waiter、
        // 创建 entry 与注册 waiter 之间不能放开锁，否则两个并发请求会同时
        // miss 后各自启动 worker —— 同 key 重复提取，且被覆盖者的取消会失效。
        let (task, waiter, created) = self.elect(&key);
        if created {
            spawn_job(
                self.runner.clone(),
                self.inner.clone(),
                state.clone(),
                request,
                key,
                task,
                permit,
            );
        } else {
            drop(permit);
        }
        waiter.wait().await
    }

    /// 在 map 临界区内加入已有任务；已取消的 entry 不复用（由调用方新建）。
    fn join_existing(&self, key: &SubtitleKey) -> Option<Waiter> {
        let mut inner = self.inner.lock();
        let entry = inner.entries.get_mut(key)?;
        if entry.task.control.is_cancelled() {
            return None;
        }
        entry.waiters += 1;
        Some(Waiter::new(
            self.inner.clone(),
            key.clone(),
            entry.task.clone(),
        ))
    }

    /// 单次持锁完成「复用已有任务 / 新建任务 + 注册 waiter」的选举。
    ///
    /// 返回 `(task, waiter, created)`；`created == true` 表示本次调用创建了任务，
    /// 由调用方负责启动 worker 并移交 permit。整个决策必须在一个临界区内，
    /// 否则同 key 的并发请求会各自启动 worker。
    fn elect(&self, key: &SubtitleKey) -> (Arc<InFlight>, Waiter, bool) {
        let mut inner = self.inner.lock();
        match inner.entries.get_mut(key) {
            Some(entry) if !entry.task.control.is_cancelled() => {
                entry.waiters += 1;
                let task = entry.task.clone();
                let waiter = Waiter::new(self.inner.clone(), key.clone(), task.clone());
                (task, waiter, false)
            }
            _ => {
                let task = Arc::new(InFlight::new());
                let waiter = Waiter::new(self.inner.clone(), key.clone(), task.clone());
                inner.entries.insert(
                    key.clone(),
                    Entry {
                        task: task.clone(),
                        waiters: 1,
                    },
                );
                (task, waiter, true)
            }
        }
    }
}

struct Waiter {
    inner: Arc<Mutex<Inner>>,
    key: SubtitleKey,
    task: Arc<InFlight>,
    armed: bool,
}

impl Waiter {
    fn new(inner: Arc<Mutex<Inner>>, key: SubtitleKey, task: Arc<InFlight>) -> Self {
        Self {
            inner,
            key,
            task,
            armed: true,
        }
    }

    async fn wait(mut self) -> JobResult {
        loop {
            // 先注册再检查：否则 worker 可能在“检查为 None”与 await 之间完成，
            // 通知就丢了。结果本身保留在 InFlight 中，注册后必定能观察到。
            let notified = self.task.ready.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(outcome) = self.task.outcome() {
                self.armed = false;
                return outcome;
            }
            notified.await;
        }
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        let mut inner = self.inner.lock();
        let Some(entry) = inner.entries.get_mut(&self.key) else {
            return;
        };
        if !Arc::ptr_eq(&entry.task, &self.task) {
            return;
        }
        entry.waiters = entry.waiters.saturating_sub(1);
        if self.armed && entry.waiters == 0 {
            entry.task.control.cancel();
        }
    }
}

fn spawn_job(
    runner: Arc<dyn library::SubtitleRunner>,
    inner: Arc<Mutex<Inner>>,
    state: ApiState,
    request: SubtitleRequest,
    key: SubtitleKey,
    task: Arc<InFlight>,
    permit: OwnedSemaphorePermit,
) {
    tokio::spawn(async move {
        // guard 保证 worker panic 时 waiter 也能拿到明确错误，而不是永久等待。
        let guard = FinishOnDrop(task.clone());
        let outcome = run_job(runner, state, request, &key, task.control.clone(), permit).await;
        task.finish(outcome);
        drop(guard);
        let mut entries = inner.lock();
        let current_is_self = entries
            .entries
            .get(&key)
            .is_some_and(|entry| Arc::ptr_eq(&entry.task, &task));
        if current_is_self {
            entries.entries.remove(&key);
        }
    });
}

/// worker 持 permit 与 LibraryLease 直到 blocking 提取真正结束。
async fn run_job(
    runner: Arc<dyn library::SubtitleRunner>,
    state: ApiState,
    request: SubtitleRequest,
    key: &SubtitleKey,
    job_control: library::WorkControl,
    permit: OwnedSemaphorePermit,
) -> JobResult {
    let lease = match state.acquire_path_work(&request.source_path, request.kind) {
        Ok(lease) => lease,
        Err(error) => {
            tracing::warn!(%error, path = %request.source_path.display(), "字幕请求无法获取 Library lease");
            return Err(SubtitleServiceError::Cancelled);
        }
    };
    let library_control = lease.as_ref().map(|lease| lease.control().clone());
    let leased = Arc::new(LeasedSubtitleRunner {
        inner: runner,
        lease: lease.map(Arc::new),
        source_path: request.source_path.clone(),
        expected_version: key.source_version.clone(),
    });
    let cache_dir = subtitle_cache_dir(request.ledger_id, &key.source_version);
    let source_path = request.source_path;
    let verify_path = source_path.clone();
    let tracks = request.tracks;
    let (index, wants_vtt) = (request.index, request.wants_vtt);
    let job = job_control.clone();
    let mut worker = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        library::deliver_subtitle_controlled(
            &tracks,
            index,
            wants_vtt,
            Some(&source_path),
            Some(&cache_dir),
            &job,
            leased.as_ref(),
        )
    });

    let deadline = Instant::now() + JOB_DEADLINE;
    let monitor = {
        let job = job_control.clone();
        let library = library_control.clone();
        async move {
            loop {
                if job.is_cancelled() {
                    return None;
                }
                if library
                    .as_ref()
                    .is_some_and(|control| control.is_cancelled())
                {
                    job.cancel();
                    return Some(SubtitleServiceError::Cancelled);
                }
                if Instant::now() >= deadline {
                    job.cancel();
                    return Some(SubtitleServiceError::Timeout);
                }
                tokio::time::sleep(JOB_POLL).await;
            }
        }
    };
    tokio::pin!(monitor);
    let joined = tokio::select! {
        joined = &mut worker => joined,
        reason = &mut monitor => {
            // 先让被取消的 blocking 提取真正结束，再报告结果。
            let _ = worker.await;
            return Err(reason.unwrap_or(SubtitleServiceError::Cancelled));
        }
    };
    match joined {
        // 成功路径同样要复核：暖缓存与外挂字幕直读都不经过 publish_file，
        // 只在发布时校验会放过“key 捕获后源文件已被替换”的旧结果。
        Ok(Ok(payload)) => {
            if superseded(&job_control, library_control.as_ref(), &verify_path, key) {
                Err(SubtitleServiceError::Cancelled)
            } else {
                Ok(payload)
            }
        }
        Ok(Err(error)) => {
            if superseded(&job_control, library_control.as_ref(), &verify_path, key) {
                Err(SubtitleServiceError::Cancelled)
            } else {
                Err(SubtitleServiceError::Delivery(error))
            }
        }
        Err(join_error) => Err(SubtitleServiceError::Worker(join_error.to_string())),
    }
}

/// 请求已失效：任务被取消、Library 被冻结，或 key 捕获之后源文件已变。
fn superseded(
    job_control: &library::WorkControl,
    library_control: Option<&library::WorkControl>,
    verify_path: &Path,
    key: &SubtitleKey,
) -> bool {
    job_control.is_cancelled()
        || library_control.is_some_and(|control| control.is_cancelled())
        || crate::fingerprint_job::current_source_version(verify_path) != key.source_version
}

/// 发布前重算 source_version，并在 Library lease 的 publish 临界区内 rename。
struct LeasedSubtitleRunner {
    inner: Arc<dyn library::SubtitleRunner>,
    lease: Option<Arc<crate::library_lifecycle::LibraryWorkLease>>,
    source_path: PathBuf,
    expected_version: String,
}

impl library::SubtitleRunner for LeasedSubtitleRunner {
    fn output(
        &self,
        command: &mut std::process::Command,
        control: &library::WorkControl,
    ) -> Result<std::process::Output, library::LibraryError> {
        self.inner.output(command, control)
    }

    fn publish_file(
        &self,
        staged: &Path,
        target: &Path,
        control: &library::WorkControl,
    ) -> Result<(), String> {
        if crate::fingerprint_job::current_source_version(&self.source_path)
            != self.expected_version
        {
            return Err("source changed before publish".into());
        }
        match &self.lease {
            Some(lease) => lease.publish(|| self.inner.publish_file(staged, target, control)),
            None => self.inner.publish_file(staged, target, control),
        }
    }
}

/// `{temp}/crawler-media-subtitles/{canonical-ledger}/{sha256(source_version)}`
pub fn subtitle_cache_dir(ledger_id: domain::LedgerId, source_version: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(source_version.as_bytes());
    std::env::temp_dir()
        .join(CACHE_ROOT)
        .join(ledger_id.to_string())
        .join(format!("{digest:x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> SubtitleKey {
        SubtitleKey {
            ledger: domain::LedgerId::new(),
            source_version: "v1".into(),
            index: 2,
            wants_vtt: true,
        }
    }

    /// R1 回归：worker 可能早于第一个 waiter 完成。结果保留在 `InFlight` 内，
    /// 晚注册的 waiter 仍必须拿到它，而不是永久等待。
    ///
    /// 旧实现用 `watch::Sender::send`：零 receiver 时 send 失败且不保存值，
    /// 而 sender 仍被任务 Arc 保活，waiter 既读不到值也等不到关闭。
    #[test]
    fn waiter_observes_an_outcome_published_before_it_registered() {
        let inner = Arc::new(Mutex::new(Inner::default()));
        let task = Arc::new(InFlight::new());
        let key = test_key();
        inner.lock().entries.insert(
            key.clone(),
            Entry {
                task: task.clone(),
                waiters: 1,
            },
        );

        // worker 先结束（旧实现在这一步丢值）。
        task.finish(Err(SubtitleServiceError::Busy));

        let waiter = Waiter::new(inner, key, task);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let observed = runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(2), waiter.wait()).await
        });
        let observed = observed.expect("晚注册的 waiter 必须观察到已保留的终态");
        assert!(matches!(observed, Err(SubtitleServiceError::Busy)));
    }

    /// 终态只发布一次：正常完成后再触发 Drop guard 不得覆盖结果。
    #[test]
    fn finish_is_idempotent() {
        let task = InFlight::new();
        task.finish(Err(SubtitleServiceError::Busy));
        task.finish(Err(SubtitleServiceError::Timeout));
        assert!(matches!(
            task.outcome(),
            Some(Err(SubtitleServiceError::Busy))
        ));
    }

    struct NoopRunner;

    impl library::SubtitleRunner for NoopRunner {
        fn output(
            &self,
            _command: &mut std::process::Command,
            _control: &library::WorkControl,
        ) -> Result<std::process::Output, library::LibraryError> {
            Err(library::LibraryError::Probe("noop runner".into()))
        }
    }

    /// R2 回归：同一 key 的并发选举只能创建一个任务。
    ///
    /// 旧实现把「最终查找已有 entry」与「插入新 entry」分成两次加锁，两个调用者
    /// 可以同时 miss 后各自插入并启动 worker（同 key 重复提取，且被覆盖者的取消
    /// 因 Arc 不符而失效）。这里断言必须**恒为 1** 的不变量。
    #[test]
    fn concurrent_elections_create_exactly_one_task() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        const THREADS: usize = 8;
        let service = Arc::new(SubtitleDelivery::new(Arc::new(NoopRunner), THREADS));
        let key = test_key();
        let barrier = Arc::new(std::sync::Barrier::new(THREADS));
        let created = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let service = service.clone();
                let key = key.clone();
                let barrier = barrier.clone();
                let created = created.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let (_task, waiter, is_new) = service.elect(&key);
                    if is_new {
                        created.fetch_add(1, Ordering::SeqCst);
                    }
                    // waiter 必须活到所有选举结束：最后一个 waiter 离开会取消任务，
                    // 后续 elect 会看到已取消 entry 而再建一个，干扰断言。
                    waiter
                })
            })
            .collect();
        let waiters: Vec<Waiter> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        assert_eq!(
            created.load(Ordering::SeqCst),
            1,
            "同一 key 的并发选举只能创建一个提取任务"
        );
        drop(waiters);
    }
}
