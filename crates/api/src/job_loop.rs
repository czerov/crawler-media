use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::{collections::HashMap, sync::Arc};

use jobs::{Job, JobError, Runner};
use parking_lot::Mutex;
use tokio::task::JoinHandle;

use crate::management::ApiState;
use crate::worker::StateRunner;

/// 控制后台 job_loop 的生命周期与正在运行的 blocking worker。
#[derive(Default)]
pub struct JobLoopControl {
    stopping: AtomicBool,
    active_workers: Mutex<Vec<JoinHandle<()>>>,
}

impl JobLoopControl {
    pub fn new() -> Self {
        Self {
            stopping: AtomicBool::new(false),
            active_workers: Mutex::new(Vec::new()),
        }
    }

    pub fn request_stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    pub fn track_worker(&self, handle: JoinHandle<()>) {
        let mut workers = self.active_workers.lock();
        workers.retain(|h| !h.is_finished());
        workers.push(handle);
    }

    pub async fn drain(&self) {
        let handles = {
            let mut workers = self.active_workers.lock();
            std::mem::take(&mut *workers)
        };
        for handle in handles {
            let _ = handle.await;
        }
    }
}

pub struct JobLoopHandle {
    loop_task: JoinHandle<()>,
    control: Arc<JobLoopControl>,
    writable: Arc<std::sync::atomic::AtomicBool>,
}

impl JobLoopHandle {
    pub fn new(loop_task: JoinHandle<()>, control: Arc<JobLoopControl>) -> Self {
        Self {
            loop_task,
            control,
            writable: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    /// G-M5: true when another live instance owns the data directory, so this
    /// instance runs no background jobs (HTTP stays available, read-only).
    pub fn is_read_only(&self) -> bool {
        !self.writable.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn abort(&self) {
        self.control.request_stop();
        self.loop_task.abort();
    }

    pub async fn shutdown(self) -> Result<(), String> {
        self.control.request_stop();
        self.loop_task.abort();
        let _ = self.loop_task.await;
        self.control.drain().await;
        Ok(())
    }
}

pub fn spawn_job_loop(state: ApiState) -> JobLoopHandle {
    let control = state.job_loop_control();
    spawn_job_loop_with_control(state, Duration::from_secs(1), unix_now, control)
}

pub fn spawn_job_loop_with(
    state: ApiState,
    interval: Duration,
    clock: impl Fn() -> i64 + Send + 'static,
) -> JobLoopHandle {
    let control = state.job_loop_control();
    spawn_job_loop_with_control(state, interval, clock, control)
}

pub fn spawn_job_loop_with_control(
    state: ApiState,
    interval: Duration,
    clock: impl Fn() -> i64 + Send + 'static,
    control: Arc<JobLoopControl>,
) -> JobLoopHandle {
    // G-M5 / H-1: only the instance holding the data-directory lease runs
    // background jobs. A read-only instance retries the lease every TTL/2
    // (a crashed holder's lease expires), and an instance whose lease was
    // taken over stops its jobs and goes back to retrying.
    let writable = state.instance_writable.clone();
    let first = crate::instance_lease::acquire(&state.store);
    writable.store(first.is_ok(), std::sync::atomic::Ordering::SeqCst);
    if let Err(error) = &first {
        tracing::error!(%error, "实例锁冲突：本实例暂不运行后台任务（只读模式），将定期重试");
    }
    let clock: Arc<Mutex<Box<dyn Fn() -> i64 + Send>>> = Arc::new(Mutex::new(Box::new(clock)));
    let loop_control = control.clone();
    let loop_writable = writable.clone();
    let loop_task = tokio::spawn(async move {
        let mut next = Some(first);
        'lease: loop {
            let attempt = match next.take() {
                Some(attempt) => attempt,
                None => crate::instance_lease::acquire(&state.store),
            };
            let lease = match attempt {
                Ok(lease) => lease,
                Err(_) => {
                    loop_writable.store(false, std::sync::atomic::Ordering::SeqCst);
                    if loop_control.is_stopping() {
                        return;
                    }
                    tokio::time::sleep(crate::instance_lease::retry_interval()).await;
                    continue 'lease;
                }
            };
            if !loop_writable.swap(true, std::sync::atomic::Ordering::SeqCst) {
                tracing::info!("已获取实例锁，恢复后台任务");
            }
            loop {
                if loop_control.is_stopping() {
                    return;
                }
                if lease.as_ref().is_some_and(|lease| lease.is_lost()) {
                    loop_writable.store(false, std::sync::atomic::Ordering::SeqCst);
                    loop_control.drain().await;
                    drop(lease);
                    tokio::time::sleep(crate::instance_lease::retry_interval()).await;
                    continue 'lease;
                }
            let now = read_clock(&clock);
            if let Err(error) = state.probe.dispatch_due(now.saturating_mul(1000), 32) {
                tracing::error!(%error, now, "【媒体探测】派发到期阶段失败");
            }
            // tick 内 runner 会同步执行 worker（订阅搜索 → Indexer 抓 PT 站点，
            // 同步 ureq 阻塞 1-8s）。直接在 async 任务里跑会占满 tokio 工作
            // 线程，导致所有 HTTP 请求（含无锁的 /health）排队超时——
            // 表现就是「正在连接服务…」+ me 不返回。挪到 spawn_blocking 池。
            let state_clone = state.clone();
            let completion_clock = Arc::clone(&clock);
            let worker_handle = tokio::task::spawn_blocking(move || {
                if state_clone.job_loop_control().is_stopping() {
                    return;
                }
                if let Err(error) = run_pending_jobs_with_clock(
                    &state_clone,
                    now,
                    || read_clock(&completion_clock),
                    TickSource::Background,
                ) {
                    tracing::error!(%error, now, "后台任务轮询失败");
                }
                // 播放会话心跳超时回收：停止上报的会话落成播放记录。
                let _ = state_clone
                    .store
                    .lock()
                    .sweep_stale_sessions(now, crate::http::playback::session_timeout_secs());
            });
            loop_control.track_worker(worker_handle);
            tokio::time::sleep(interval).await;
            }
        }
    });

    let mut handle = JobLoopHandle::new(loop_task, control);
    handle.writable = writable;
    handle
}

/// 一轮 tick 的来源：决定维护任务是否参与本轮调度。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TickSource {
    /// 正式后台 job_loop：维护任务正常执行。
    Background,
    /// `/jobs/tick` 开发入口：维护任务需要自己的 Queue 短锁，而该入口的 runner 调用
    /// 约定是持锁调用（`Queue::tick`），因此本轮不执行、只报告。
    DevHttp,
}

pub(crate) struct TickRun {
    pub finished: Vec<Job>,
    /// 仅开发入口非空：本轮被跳过的维护任务 kind。
    pub skipped_maintenance: Vec<String>,
}

/// Claim work while holding the queue lock, execute it without the lock, then
/// briefly reacquire the queue to persist each outcome. Network-bound runners
/// must never make `/jobs` readers wait for the full Job duration.
pub(crate) fn run_pending_jobs_with_clock(
    state: &ApiState,
    now: i64,
    completion_clock: impl Fn() -> i64,
    source: TickSource,
) -> Result<TickRun, JobError> {
    let (claimed, skipped_maintenance) = claim_pending_jobs(state, now, source)?;
    let runner = StateRunner {
        state: state.clone(),
    };
    let mut finished = Vec::with_capacity(claimed.len());
    let active_guards = claimed
        .iter()
        .map(|job| ActiveAttemptGuard::new(Arc::clone(&state.active_job_attempts), job.clone()))
        .collect::<Vec<_>>();
    for (job, mut active_guard) in claimed.into_iter().zip(active_guards) {
        let job_name = job
            .def_id
            .and_then(|def_id| state.jobs.lock().def_name(def_id).ok().flatten())
            .unwrap_or_else(|| job.kind.as_str().to_owned());
        tracing::info!(job_id = %job.id, job_name = %job_name, kind = ?job.kind, "后台任务开始执行");
        let result = runner.run(&job);
        let persisted =
            persist_job_result(state, &job, result, completion_clock(), &mut active_guard)?;
        if let Some(persisted) = persisted {
            finished.push(persisted);
        } else {
            tracing::warn!(job_id = %job.id, attempt = job.attempt, "忽略超时后旧执行实例的完成结果");
        }
    }
    Ok(TickRun {
        finished,
        skipped_maintenance,
    })
}

fn claim_pending_jobs(
    state: &ApiState,
    now: i64,
    source: TickSource,
) -> Result<(Vec<Job>, Vec<String>), JobError> {
    let mut active = state.active_job_attempts.lock();
    let queue = state.jobs.lock();
    let active_attempts: Vec<_> = active.values().cloned().collect();
    queue.recover_except(now, &active_attempts)?;
    let skipped_maintenance = schedule_due(&queue, now, source)?;
    let claimed = if source == TickSource::DevHttp {
        // 开发入口：只 claim 非维护任务，不取消排队中的维护任务，也不污染维护任务的最近报告
        claim_non_maintenance(&queue, now, 2)?
    } else {
        queue.claim(now, 2)?
    };
    for job in &claimed {
        active.insert(job.id, job.clone());
    }
    Ok((claimed, skipped_maintenance))
}

fn claim_non_maintenance(
    queue: &jobs::Queue,
    now: i64,
    limit: usize,
) -> Result<Vec<Job>, JobError> {
    let all = queue.claim(now, limit)?;
    let mut claimed = Vec::new();
    for job in all {
        if job.kind.is_maintenance() {
            // 将其无惩罚放回 queued 状态，不增加 attempt 也不记录错误
            if let Err(error) = queue.requeue_without_attempt(&job) {
                tracing::error!(job_id = %job.id, %error, "将维护任务放回队列失败");
            }
        } else {
            claimed.push(job);
        }
    }
    Ok(claimed)
}

/// 按 tick 来源排期。
///
/// 开发入口**不为维护 def 生成子任务**。返回被跳过的维护 kind。
fn schedule_due(
    queue: &jobs::Queue,
    now: i64,
    source: TickSource,
) -> Result<Vec<String>, JobError> {
    if source == TickSource::Background {
        queue.ensure_scheduled(now)?;
        return Ok(Vec::new());
    }
    let mut skipped = Vec::new();
    for def in queue.list_defs()? {
        if !def.enabled {
            continue;
        }
        if !def.kind.is_maintenance() {
            queue.ensure_scheduled_for(now, def.id)?;
            continue;
        }
        skipped.push(def.kind.as_str().to_string());
        tracing::warn!(
            kind = def.kind.as_str(),
            "开发入口 /jobs/tick 不执行维护任务，交给后台 job_loop"
        );
    }
    Ok(skipped)
}

fn persist_job_result(
    state: &ApiState,
    job: &Job,
    result: Result<(), String>,
    finished_at: i64,
    active_guard: &mut ActiveAttemptGuard,
) -> Result<Option<Job>, JobError> {
    // Keep the marker through the DB write; claiming uses the same lock order.
    let mut active = state.active_job_attempts.lock();
    let queue = state.jobs.lock();
    let persisted = match result {
        Ok(()) => {
            tracing::info!(job_id = %job.id, kind = ?job.kind, "后台任务执行成功");
            queue.succeed_claimed(job, finished_at)?
        }
        Err(error) => {
            tracing::error!(job_id = %job.id, kind = ?job.kind, error = %error, "后台任务执行失败");
            queue.fail_claimed(job, finished_at, &error)?
        }
    };
    if active
        .get(&job.id)
        .is_some_and(|attempt| same_attempt(attempt, job))
    {
        active.remove(&job.id);
    }
    active_guard.disarm();
    Ok(persisted)
}

type ActiveJobMap = HashMap<domain::JobId, Job>;

struct ActiveAttemptGuard {
    active: Arc<Mutex<ActiveJobMap>>,
    job: Job,
    armed: bool,
}

impl ActiveAttemptGuard {
    fn new(active: Arc<Mutex<ActiveJobMap>>, job: Job) -> Self {
        Self {
            active,
            job,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ActiveAttemptGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut active = self.active.lock();
        if active
            .get(&self.job.id)
            .is_some_and(|attempt| same_attempt(attempt, &self.job))
        {
            active.remove(&self.job.id);
        }
    }
}

fn same_attempt(left: &Job, right: &Job) -> bool {
    left.id == right.id && left.attempt == right.attempt && left.started_at == right.started_at
}

fn read_clock(clock: &Arc<Mutex<Box<dyn Fn() -> i64 + Send>>>) -> i64 {
    (clock.lock())()
}

pub fn unix_now() -> i64 {
    crate::store::unix_now()
}
