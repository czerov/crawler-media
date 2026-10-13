//! DB8 Task 4：维护任务 runner。
//!
//! 职责边界：
//!
//! - 按 `job.kind` 严格解析版本化策略；解析失败**直接失败**，绝不回退成更激进的策略。
//! - 固定本轮的 `now`（取 claim 时的 `started_at`）与 `cutoff`，避免执行期间窗口漂移。
//! - 每批只持一次短锁，批间释放；不在持 Store 锁时取 Queue 锁，反之亦然。
//! - 每轮结束把汇总报告写进 `job.progress`（成功与失败都写）。
//!
//! 数据库维护的重活跑在**独立连接**上，不在全局 Store/Queue 锁内；但检查 Library 工作
//! 只是降低冲突，不是与扫描互斥的证明，SQLite busy 与有界执行仍然必须存在。

use std::time::{Duration, Instant};

use jobs::{Job, JobStatus};

use super::database::{
    DatabaseName, MaintenanceContext, database_targets, missing_optional_targets,
};
use super::policy::{BATCH_SIZE, DatabaseMaintenancePolicy, MAX_BATCHES, MaintenancePolicy};
use super::report::{DatabaseOutcome, DatabaseReport, MaintenanceReport};
use crate::management::ApiState;

const DAY_SECS: i64 = 86_400;
/// 探测 Queue 锁是否已被当前路径持有：重试窗口很短，避免维护被前台读请求误伤。
const QUEUE_PROBE_ATTEMPTS: usize = 10;
const QUEUE_PROBE_INTERVAL: Duration = Duration::from_millis(10);

/// 维护类任务的统一入口（`worker::StateRunner` 在**不持 Queue 锁**时调用）。
pub fn run(state: &ApiState, job: &Job) -> Result<(), String> {
    let policy = MaintenancePolicy::parse(job.kind, &job.payload)?;
    refuse_when_queue_lock_held(state)?;
    // 进程内互斥：`concurrency_key` 只防同一个 def 重叠，这里覆盖不同维护 def。
    let _exclusive = state
        .maintenance_lock
        .try_lock()
        .ok_or("已有维护任务在执行，本轮不重叠")?;

    let now = job.started_at.unwrap_or_else(crate::job_loop::unix_now);
    let started = Instant::now();
    let cancelled = || job_cancelled(state, job);
    let version = match &policy {
        MaintenancePolicy::Database(policy) => policy.version,
        MaintenancePolicy::Jobs(policy) => policy.version,
        MaintenancePolicy::PlaybackHistory(policy) => policy.version,
    };
    let mut report = MaintenanceReport::new(job.kind, version);
    tracing::info!(job_id = %job.id, kind = job.kind.as_str(), "维护任务开始执行");

    let outcome = match &policy {
        MaintenancePolicy::Database(policy) => {
            run_database(state, job, policy, now, &cancelled, &mut report)
        }
        MaintenancePolicy::Jobs(policy) => {
            let cutoff = cutoff(now, policy.retention_days)?;
            report.cutoff = Some(cutoff);
            report.dry_run = policy.dry_run;
            let dry_run = policy.dry_run;
            run_batches(&mut report, dry_run, &cancelled, || {
                let queue = state.jobs.lock();
                queue
                    .prune_terminal_jobs(cutoff, BATCH_SIZE, dry_run)
                    .map(BatchCounts::from_jobs)
                    .map_err(|error| error.to_string())
            })
        }
        MaintenancePolicy::PlaybackHistory(policy) => {
            let cutoff = cutoff(now, policy.retention_days)?;
            report.cutoff = Some(cutoff);
            report.dry_run = policy.dry_run;
            let dry_run = policy.dry_run;
            run_batches(&mut report, dry_run, &cancelled, || {
                let store = state.store.lock();
                store
                    .prune_playback_logs(cutoff, BATCH_SIZE, dry_run)
                    .map(BatchCounts::from_playback)
                    .map_err(|error| error.to_string())
            })
        }
    };
    report.elapsed_ms = elapsed_ms(started);

    if report.budget_exhausted {
        tracing::warn!(
            job_id = %job.id, kind = job.kind.as_str(), batches = report.batches,
            deleted = report.deleted, "维护任务预算耗尽，剩余候选留到下一轮"
        );
    }
    if report.note.is_some() {
        tracing::warn!(job_id = %job.id, kind = job.kind.as_str(), note = ?report.note, "维护任务被降级或提前停止");
    }
    let written = write_progress(state, job, &report);
    match (outcome, written) {
        (Ok(()), Ok(())) => {
            tracing::info!(
                job_id = %job.id, kind = job.kind.as_str(), batches = report.batches,
                deleted = report.deleted, elapsed_ms = report.elapsed_ms, "维护任务执行完成"
            );
            Ok(())
        }
        (Ok(()), Err(error)) => Err(error),
        (Err(error), _) => {
            tracing::error!(job_id = %job.id, kind = job.kind.as_str(), %error, "维护任务执行失败");
            Err(error)
        }
    }
}

/// `Queue::tick` 会在持有 Queue 锁时调用 runner；本模块随后要取 Queue 短锁，重入
/// `parking_lot::Mutex` 会死锁。因此持锁路径一律明确拒绝，而不是静默执行。
fn refuse_when_queue_lock_held(state: &ApiState) -> Result<(), String> {
    for _ in 0..QUEUE_PROBE_ATTEMPTS {
        if state.jobs.try_lock().is_some() {
            return Ok(());
        }
        std::thread::sleep(QUEUE_PROBE_INTERVAL);
    }
    Err("维护任务不能在持有 Queue 锁的路径中执行（Queue::tick / 持锁 runner）；请由后台 job_loop 调度".into())
}

/// 取消信号：本次 attempt 已不再是库里的 running 行（被取消/恢复/删除）。
fn job_cancelled(state: &ApiState, job: &Job) -> bool {
    let queue = state.jobs.lock();
    match queue.get(job.id) {
        Ok(Some(current)) => current.status != JobStatus::Running || current.attempt != job.attempt,
        Ok(None) => true,
        Err(error) => {
            tracing::warn!(job_id = %job.id, %error, "读取维护任务状态失败，保守停止");
            true
        }
    }
}

fn cutoff(now: i64, retention_days: u32) -> Result<i64, String> {
    let seconds = i64::from(retention_days)
        .checked_mul(DAY_SECS)
        .ok_or("保留天数换算秒数溢出")?;
    now.checked_sub(seconds)
        .ok_or_else(|| format!("保留窗口下界溢出: now={now} seconds={seconds}"))
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn write_progress(state: &ApiState, job: &Job, report: &MaintenanceReport) -> Result<(), String> {
    let json =
        serde_json::to_string(report).map_err(|error| format!("序列化维护报告失败: {error}"))?;
    let queue = state.jobs.lock();
    queue
        .set_job_progress(job.id, &json)
        .map_err(|error| format!("写入维护报告失败: {error}"))
}

// ---------------------------------------------------------------------------
// 数据库维护
// ---------------------------------------------------------------------------

fn run_database(
    state: &ApiState,
    job: &Job,
    policy: &DatabaseMaintenancePolicy,
    now: i64,
    cancelled: &dyn Fn() -> bool,
    report: &mut MaintenanceReport,
) -> Result<(), String> {
    // 只把 owned 路径带出 Store 锁，随后立刻释放。
    let (data_dir, app_path) = {
        let store = state.store.lock();
        (
            store.data_dir().to_path_buf(),
            store.app_db_path().to_path_buf(),
        )
    };
    // 预检：Library 工作运行时跳过重型维护，但仍做轻量 optimize（报告里写明原因）。
    let library_busy = state.library_work.has_active_work();
    let vacuum_enabled = policy.vacuum_enabled && !library_busy;
    if policy.vacuum_enabled && library_busy {
        report.note =
            Some("Library 工作运行中，本轮跳过重型 VACUUM（仍执行 PRAGMA optimize）".to_string());
    }

    let mut failures = Vec::new();
    for target in database_targets(&data_dir, &app_path) {
        if cancelled() {
            report.note = Some("任务已取消，未启动下一库".to_string());
            break;
        }
        let context = MaintenanceContext {
            now,
            last_vacuum_success: read_last_vacuum(state, target.name),
            cancelled,
        };
        let result = state
            .maintenance_executor
            .execute(&target, vacuum_enabled, &context);
        let entry = match result {
            Ok(entry) => entry,
            Err(error) => {
                tracing::error!(database = target.name.as_str(), %error, "维护执行器无法打开数据库");
                DatabaseReport::failed(target.name.as_str(), error)
            }
        };
        if entry.outcome == DatabaseOutcome::Vacuumed {
            record_vacuum_success(state, target.name, now);
        }
        if entry.outcome == DatabaseOutcome::Skipped {
            tracing::warn!(
                database = entry.name, reason = ?entry.skip_reason,
                "数据库维护跳过该库"
            );
        }
        if entry.outcome == DatabaseOutcome::Failed {
            failures.push(format!(
                "{}: {}",
                entry.name,
                entry.skip_reason.clone().unwrap_or_default()
            ));
        }
        report.databases.push(entry);
    }
    for target in missing_optional_targets(&data_dir) {
        report.databases.push(DatabaseReport::skipped(
            target.name.as_str(),
            "数据库文件不存在（可选库，不创建）",
        ));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} 个数据库维护失败: {}",
            failures.len(),
            failures.join("; ")
        ))
    }
}

fn vacuum_setting_key(name: DatabaseName) -> String {
    format!("maintenance.vacuum_last_success.{}", name.as_str())
}

fn read_last_vacuum(state: &ApiState, name: DatabaseName) -> Option<i64> {
    let store = state.store.lock();
    store
        .get_setting(&vacuum_setting_key(name))
        .ok()
        .flatten()
        .and_then(|raw| raw.parse().ok())
}

/// VACUUM 成功才推进冷却；跳过/失败不写，避免"没做也当作做过"。
fn record_vacuum_success(state: &ApiState, name: DatabaseName, now: i64) {
    let store = state.store.lock();
    if let Err(error) = store.put_setting(&vacuum_setting_key(name), &now.to_string()) {
        tracing::error!(database = name.as_str(), %error, "记录 VACUUM 成功时间失败");
    }
}

// ---------------------------------------------------------------------------
// 保留批次
// ---------------------------------------------------------------------------

struct BatchCounts {
    scanned: usize,
    candidates: usize,
    deleted: usize,
    protected: usize,
    has_more: bool,
}

impl BatchCounts {
    fn from_jobs(batch: jobs::RetentionBatch) -> Self {
        Self {
            scanned: batch.scanned,
            candidates: batch.candidates,
            deleted: batch.deleted,
            protected: batch.protected,
            has_more: batch.has_more,
        }
    }

    fn from_playback(batch: store::PlaybackRetentionBatch) -> Self {
        Self {
            scanned: batch.scanned,
            candidates: batch.candidates,
            deleted: batch.deleted,
            protected: batch.protected,
            has_more: batch.has_more,
        }
    }
}

/// 分批执行并累计报告。
///
/// 停止条件：
/// - **dry_run**：只检视一批，然后把 `has_more` 如实映射成 `budget_exhausted`。
///   试运行不删除，再取下一批只会重复检视同一批行（会虚报候选数），所以不能循环；
///   但也**不能**把 `has_more` 吞掉说"已完成"——用户开 dry_run 就是为了先看规模。
/// - **实删**：`!has_more`（窗口已只含可删行，所以这是真正的"没有更多可删候选"），
///   或 `deleted == 0`（本轮已无可删行），或批数用尽。
fn run_batches(
    report: &mut MaintenanceReport,
    dry_run: bool,
    cancelled: &dyn Fn() -> bool,
    mut next: impl FnMut() -> Result<BatchCounts, String>,
) -> Result<(), String> {
    let mut batches = 0usize;
    let mut more_remains = false;
    while batches < MAX_BATCHES {
        if cancelled() {
            report.note = Some("任务已取消，未启动下一批".to_string());
            break;
        }
        let batch = next()?;
        report.scanned += batch.scanned;
        report.candidates += batch.candidates;
        report.deleted += batch.deleted;
        report.protected += batch.protected;
        batches += 1;
        if dry_run {
            more_remains = batch.has_more;
            if batch.has_more {
                report.note = Some("试运行只检视一批；还有更多可删候选未计入".to_string());
            }
            break;
        }
        if !batch.has_more || batch.deleted == 0 {
            more_remains = false;
            break;
        }
        more_remains = true;
    }
    report.batches = batches;
    report.budget_exhausted = more_remains && (dry_run || batches >= MAX_BATCHES);
    Ok(())
}
