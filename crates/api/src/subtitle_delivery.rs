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
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

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

struct InFlight {
    result: watch::Sender<Option<JobResult>>,
    control: library::WorkControl,
}

impl InFlight {
    fn new() -> Self {
        let (result, _receiver) = watch::channel(None);
        Self {
            result,
            control: library::WorkControl::default(),
        }
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

        if let Some(waiter) = self.join_existing(&key) {
            return waiter.wait().await;
        }
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| SubtitleServiceError::Busy)?;
        if let Some(waiter) = self.join_existing(&key) {
            drop(permit);
            return waiter.wait().await;
        }
        let task = Arc::new(InFlight::new());
        {
            let mut inner = self.inner.lock();
            inner.entries.insert(
                key.clone(),
                Entry {
                    task: task.clone(),
                    waiters: 1,
                },
            );
        }
        spawn_job(
            self.runner.clone(),
            self.inner.clone(),
            state.clone(),
            request,
            key.clone(),
            task.clone(),
            permit,
        );
        Waiter::new(self.inner.clone(), key, task).wait().await
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
}

struct Waiter {
    inner: Arc<Mutex<Inner>>,
    key: SubtitleKey,
    task: Arc<InFlight>,
    result: watch::Receiver<Option<JobResult>>,
    armed: bool,
}

impl Waiter {
    fn new(inner: Arc<Mutex<Inner>>, key: SubtitleKey, task: Arc<InFlight>) -> Self {
        let result = task.result.subscribe();
        Self {
            inner,
            key,
            task,
            result,
            armed: true,
        }
    }

    async fn wait(mut self) -> JobResult {
        loop {
            if let Some(value) = self.result.borrow().clone() {
                self.armed = false;
                return value;
            }
            if self.result.changed().await.is_err() {
                self.armed = false;
                return Err(SubtitleServiceError::Worker(
                    "subtitle worker dropped".into(),
                ));
            }
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
        let outcome = run_job(runner, state, request, &key, task.control.clone(), permit).await;
        let _ = task.result.send(Some(outcome));
        let mut guard = inner.lock();
        let current_is_self = guard
            .entries
            .get(&key)
            .is_some_and(|entry| Arc::ptr_eq(&entry.task, &task));
        if current_is_self {
            guard.entries.remove(&key);
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
        Ok(Ok(payload)) => Ok(payload),
        Ok(Err(error)) => {
            let source_changed =
                crate::fingerprint_job::current_source_version(&verify_path) != key.source_version;
            let library_cancelled = library_control
                .as_ref()
                .is_some_and(|control| control.is_cancelled());
            if job_control.is_cancelled() || library_cancelled || source_changed {
                Err(SubtitleServiceError::Cancelled)
            } else {
                Err(SubtitleServiceError::Delivery(error))
            }
        }
        Err(join_error) => Err(SubtitleServiceError::Worker(join_error.to_string())),
    }
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
