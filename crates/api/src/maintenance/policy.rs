//! 维护任务的版本化策略：类型、默认值、范围校验。
//!
//! 策略只有**一份**——存在该维护 def 的 payload 里。settings 不再存第二份副本，
//! 避免"两个事实来源"（DB11 的教训）。
//!
//! 解析一律**严格**：未知字段、未知版本、缺失字段、越界值、损坏 JSON 全部拒绝。
//! 维护配置宁可拒绝执行，也不能回退成更激进的删除策略。

use jobs::JobKind;
use serde::Deserialize;

/// 维护 payload 的 schema 版本。升版本时必须同时提供旧版本读取路径。
pub const POLICY_VERSION: u32 = 1;

pub const DEFAULT_RETENTION_DAYS: u32 = 90;
pub const MIN_RETENTION_DAYS: u32 = 1;
pub const MAX_RETENTION_DAYS: u32 = 36_500;

pub const DEFAULT_DATABASE_INTERVAL_SECS: u64 = 21_600;
pub const DEFAULT_JOBS_INTERVAL_SECS: u64 = 86_400;
pub const DEFAULT_PLAYBACK_INTERVAL_SECS: u64 = 86_400;
pub const DEFAULT_DATA_RETENTION_INTERVAL_SECS: u64 = 86_400;

/// DB-05 三类历史的默认窗口（设计固定 90 天）。
pub const DEFAULT_DATA_RETENTION_DAYS: u32 = 90;

/// 维护任务的执行周期范围（只对维护任务生效，其他任务契约不变）。
pub const MIN_MAINTENANCE_INTERVAL_SECS: u64 = 3_600;
pub const MAX_MAINTENANCE_INTERVAL_SECS: u64 = 31_536_000;

/// 维护任务的执行超时。VACUUM 是重活，留足余量。
pub const MAINTENANCE_TIMEOUT_SECS: u32 = 900;

/// 服务端固定的安全预算，不暴露给用户任意扩大。
pub const BATCH_SIZE: usize = 500;
pub const MAX_BATCHES: usize = 20;

/// B3 §5.1：单次 retention「最多 20 批或 2 秒（先到先停）」。单条 SQL 步不可
/// 中断，所以时间预算只在批间判定，日志区分预算与实际 elapsed。
pub const MAX_BATCH_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// VACUUM 准入阈值（固定，不可配置）：冷却、空闲空间、空闲页比例。
pub const VACUUM_MIN_INTERVAL_SECS: u64 = 604_800;
pub const VACUUM_MIN_FREE_BYTES: u64 = 64 * 1024 * 1024;
pub const VACUUM_MIN_FREE_RATIO_PERCENT: u32 = 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseMaintenancePolicy {
    pub version: u32,
    /// 重型空间回收。默认 `false`——不做"每 6 小时无条件 VACUUM"。
    pub vacuum_enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobsRetentionPolicy {
    pub version: u32,
    pub retention_days: u32,
    pub dry_run: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackHistoryRetentionPolicy {
    pub version: u32,
    pub retention_days: u32,
    pub dry_run: bool,
}

/// DB-05：分类历史保留。三个窗口各自独立；策略只存这份 payload。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataRetentionPolicy {
    pub version: u32,
    pub dry_run: bool,
    pub qoe_days: u32,
    pub transient_cache_days: u32,
    pub probe_days: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MaintenancePolicy {
    Database(DatabaseMaintenancePolicy),
    Jobs(JobsRetentionPolicy),
    PlaybackHistory(PlaybackHistoryRetentionPolicy),
    DataRetention(DataRetentionPolicy),
}

impl MaintenancePolicy {
    /// 按任务类型解析并校验 payload。
    pub fn parse(kind: JobKind, json: &str) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|error| format!("维护配置不是合法 JSON: {error}"))?;
        let version = value
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "维护配置缺少 version".to_string())?;
        if version != u64::from(POLICY_VERSION) {
            return Err(format!(
                "不支持的维护配置版本 {version}（本程序支持 {POLICY_VERSION}）"
            ));
        }
        match kind {
            JobKind::DatabaseMaintenance => {
                let raw: RawDatabase = decode(value)?;
                Ok(Self::Database(DatabaseMaintenancePolicy {
                    version: POLICY_VERSION,
                    vacuum_enabled: raw.vacuum_enabled,
                }))
            }
            JobKind::JobsRetention => {
                let raw: RawRetention = decode(value)?;
                validate_retention_days(raw.retention_days)?;
                Ok(Self::Jobs(JobsRetentionPolicy {
                    version: POLICY_VERSION,
                    retention_days: raw.retention_days,
                    dry_run: raw.dry_run,
                }))
            }
            JobKind::PlaybackHistoryRetention => {
                let raw: RawRetention = decode(value)?;
                validate_retention_days(raw.retention_days)?;
                Ok(Self::PlaybackHistory(PlaybackHistoryRetentionPolicy {
                    version: POLICY_VERSION,
                    retention_days: raw.retention_days,
                    dry_run: raw.dry_run,
                }))
            }
            JobKind::DataRetention => {
                let raw: RawDataRetention = decode(value)?;
                validate_retention_days(raw.qoe_days)?;
                validate_retention_days(raw.transient_cache_days)?;
                validate_retention_days(raw.probe_days)?;
                Ok(Self::DataRetention(DataRetentionPolicy {
                    version: POLICY_VERSION,
                    dry_run: raw.dry_run,
                    qoe_days: raw.qoe_days,
                    transient_cache_days: raw.transient_cache_days,
                    probe_days: raw.probe_days,
                }))
            }
            other => Err(format!("{} 不是维护任务", other.as_str())),
        }
    }

    pub fn retention_days(&self) -> Option<u32> {
        match self {
            Self::Jobs(policy) => Some(policy.retention_days),
            Self::PlaybackHistory(policy) => Some(policy.retention_days),
            // 分类保留有三个独立窗口，不存在单一 `retention_days`。
            Self::DataRetention(_) | Self::Database(_) => None,
        }
    }

    pub fn dry_run(&self) -> Option<bool> {
        match self {
            Self::Jobs(policy) => Some(policy.dry_run),
            Self::PlaybackHistory(policy) => Some(policy.dry_run),
            Self::DataRetention(policy) => Some(policy.dry_run),
            Self::Database(_) => None,
        }
    }
}

fn decode<T: for<'de> Deserialize<'de>>(value: serde_json::Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| format!("维护配置字段不合法: {error}"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDatabase {
    #[allow(dead_code)]
    version: u32,
    vacuum_enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDataRetention {
    #[allow(dead_code)]
    version: u32,
    dry_run: bool,
    qoe_days: u32,
    transient_cache_days: u32,
    probe_days: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRetention {
    #[allow(dead_code)]
    version: u32,
    retention_days: u32,
    dry_run: bool,
}

pub fn validate_retention_days(days: u32) -> Result<(), String> {
    if !(MIN_RETENTION_DAYS..=MAX_RETENTION_DAYS).contains(&days) {
        return Err(format!(
            "保留天数必须在 {MIN_RETENTION_DAYS}..={MAX_RETENTION_DAYS} 之间（0 不代表永久保留，永久保留请关闭该任务）: {days}"
        ));
    }
    Ok(())
}

pub fn validate_maintenance_interval(secs: u64) -> Result<(), String> {
    if !(MIN_MAINTENANCE_INTERVAL_SECS..=MAX_MAINTENANCE_INTERVAL_SECS).contains(&secs) {
        return Err(format!(
            "维护任务周期必须在 {MIN_MAINTENANCE_INTERVAL_SECS}..={MAX_MAINTENANCE_INTERVAL_SECS} 秒之间: {secs}"
        ));
    }
    Ok(())
}
