//! DB8 维护任务：版本化策略、默认注册、数据库执行器与 runner。
//!
//! 调度/开关继续由现有 `job_defs` 承担，本模块负责"默认长什么样"、"payload 怎么
//! 解析"以及"一轮执行怎么做、怎么报告"。

pub mod policy;

mod database;
mod report;
mod runner;

pub use database::{
    ConfiguredSqliteExecutor, DatabaseExecutor, DatabaseName, DatabaseTarget, MaintenanceContext,
    PER_DATABASE_LIMIT, SqliteExecutor, SqliteLimits, VACUUM_DISK_HEADROOM_BYTES, database_targets,
};
pub use policy::{
    BATCH_SIZE, DEFAULT_DATABASE_INTERVAL_SECS, DEFAULT_JOBS_INTERVAL_SECS,
    DEFAULT_PLAYBACK_INTERVAL_SECS, DEFAULT_RETENTION_DAYS, DatabaseMaintenancePolicy,
    JobsRetentionPolicy, MAINTENANCE_TIMEOUT_SECS, MAX_BATCHES, MAX_MAINTENANCE_INTERVAL_SECS,
    MAX_RETENTION_DAYS, MIN_MAINTENANCE_INTERVAL_SECS, MIN_RETENTION_DAYS, MaintenancePolicy,
    POLICY_VERSION, PlaybackHistoryRetentionPolicy, VACUUM_MIN_FREE_BYTES,
    VACUUM_MIN_FREE_RATIO_PERCENT, VACUUM_MIN_INTERVAL_SECS, validate_maintenance_interval,
    validate_retention_days,
};
pub use report::{DatabaseOutcome, DatabaseReport, MaintenanceReport};
pub use runner::{check_maintenance_stop, run, MaintenanceStop};

use jobs::{JobKind, NewDef, Queue, Schedule};

/// 一个维护任务的默认形态。默认开关与频率**只在首次注册时使用**；
/// `ensure_def` 对已存在的 def 原样返回，因此用户改动在重启后保留。
struct DefSpec {
    kind: JobKind,
    name: &'static str,
    enabled: bool,
    interval_secs: u64,
    concurrency_key: &'static str,
}

const DEFS: [DefSpec; 3] = [
    DefSpec {
        kind: JobKind::DatabaseMaintenance,
        name: "数据库维护",
        enabled: true,
        interval_secs: DEFAULT_DATABASE_INTERVAL_SECS,
        concurrency_key: "maintenance:database",
    },
    DefSpec {
        kind: JobKind::JobsRetention,
        name: "任务历史清理",
        enabled: true,
        interval_secs: DEFAULT_JOBS_INTERVAL_SECS,
        concurrency_key: "maintenance:jobs_retention",
    },
    DefSpec {
        kind: JobKind::PlaybackHistoryRetention,
        name: "播放历史清理",
        // 删的是用户能看见的"最近播放"，默认关闭，由用户显式开启。
        enabled: false,
        interval_secs: DEFAULT_PLAYBACK_INTERVAL_SECS,
        concurrency_key: "maintenance:playback_history",
    },
];

/// 维护 payload 的默认值。必须能被 [`MaintenancePolicy::parse`] 原样接受。
pub fn default_payload(kind: JobKind) -> Option<String> {
    let value = match kind {
        JobKind::DatabaseMaintenance => serde_json::json!({
            "version": POLICY_VERSION,
            "vacuum_enabled": false,
        }),
        JobKind::JobsRetention | JobKind::PlaybackHistoryRetention => serde_json::json!({
            "version": POLICY_VERSION,
            "retention_days": DEFAULT_RETENTION_DAYS,
            "dry_run": false,
        }),
        _ => return None,
    };
    Some(value.to_string())
}

/// 注册三类维护任务。幂等：已存在的 def（含用户改过的开关/频率/payload）原样保留。
pub fn seed_maintenance_defs(queue: &Queue) -> Result<(), jobs::JobError> {
    for spec in &DEFS {
        let Some(payload) = default_payload(spec.kind) else {
            continue;
        };
        queue.ensure_def(NewDef {
            kind: spec.kind,
            name: spec.name.to_string(),
            enabled: spec.enabled,
            // 关闭的任务也保留非空 schedule：现有定时任务页只显示 schedule 非空的 def。
            schedule: Some(Schedule::Interval {
                secs: spec.interval_secs,
            }),
            payload,
            timeout_secs: Some(MAINTENANCE_TIMEOUT_SECS),
            concurrency_key: Some(spec.concurrency_key.to_string()),
        })?;
    }
    Ok(())
}
