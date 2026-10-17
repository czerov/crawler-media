//! DB-05 分类历史保留的执行器。
//!
//! 复用现有维护合同：策略来自 def payload、批间检查 disabled/cancel/shutdown、
//! 每批只持一次 Store 锁、报告写 `job.progress`。不建平行 scheduler。
//!
//! 预算：500 行/批，单轮最多 20 批或 2 秒（先到先停）。单条 SQL 不可中断，
//! 所以时间预算只在批间判定，不能据此宣称扫描或持锁时间有硬上界。
//!
//! 游标按类别持久化在 app settings：`maintenance.data_retention.cursor.{class}`。
//! 批提交后写游标；崩溃只会导致重检视（安全），不会跳过候选。cycle 完成即清游标。

use std::time::Instant;

use jobs::Job;
use store::{DataRetentionCursor, RetentionClass};

use super::policy::{BATCH_SIZE, DataRetentionPolicy, MAX_BATCHES, MAX_BATCH_BUDGET};
use super::report::{DataRetentionClassReport, MaintenanceReport};
use super::runner::MaintenanceStop;
use crate::management::ApiState;

const DAY_MS: i64 = 86_400_000;

pub const DATA_RETENTION_CLASSES: [RetentionClass; 3] = [
    RetentionClass::Qoe,
    RetentionClass::TransientCatalog,
    RetentionClass::ProbeHistory,
];

pub fn cutoff_ms_for_days(now_secs: i64, days: u32) -> Result<i64, String> {
    let now_ms = now_secs
        .checked_mul(1000)
        .ok_or_else(|| format!("毫秒时间溢出: now={now_secs}"))?;
    let span = i64::from(days)
        .checked_mul(DAY_MS)
        .ok_or("保留天数换算毫秒溢出")?;
    now_ms
        .checked_sub(span)
        .ok_or_else(|| format!("保留窗口下界溢出: now_ms={now_ms} span_ms={span}"))
}

fn class_days(policy: &DataRetentionPolicy, class: RetentionClass) -> u32 {
    match class {
        RetentionClass::Qoe => policy.qoe_days,
        RetentionClass::TransientCatalog => policy.transient_cache_days,
        RetentionClass::ProbeHistory => policy.probe_days,
    }
}

fn cursor_key(class: RetentionClass) -> String {
    format!("maintenance.data_retention.cursor.{}", class.as_str())
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct StoredCursor {
    cutoff_ms: i64,
    at_ms: i64,
    id: String,
}

/// 读取已持久化游标。只有存储的 cutoff 不比本轮新（即仍然更保守）时才继续
/// 旧 cycle；否则从新 cutoff 从头开始。坏 JSON 视为没有游标（只会多检视，不会多删）。
pub(crate) fn load_cursor(
    state: &ApiState,
    class: RetentionClass,
    requested_cutoff_ms: i64,
) -> (i64, Option<DataRetentionCursor>) {
    let key = cursor_key(class);
    let raw = {
        let store = state.store.lock();
        store.get_setting(&key)
    };
    let Ok(Some(raw)) = raw else {
        return (requested_cutoff_ms, None);
    };
    let Ok(stored) = serde_json::from_str::<StoredCursor>(&raw) else {
        tracing::warn!(class = class.as_str(), "数据保留游标损坏，本轮从新 cutoff 开始");
        return (requested_cutoff_ms, None);
    };
    if stored.cutoff_ms <= requested_cutoff_ms {
        (
            stored.cutoff_ms,
            Some(DataRetentionCursor {
                at_ms: stored.at_ms,
                id: stored.id,
            }),
        )
    } else {
        (requested_cutoff_ms, None)
    }
}

fn persist_cursor(
    state: &ApiState,
    class: RetentionClass,
    cutoff_ms: i64,
    cursor: Option<&DataRetentionCursor>,
) -> Result<(), String> {
    let key = cursor_key(class);
    let store = state.store.lock();
    match cursor {
        Some(cursor) => {
            let stored = StoredCursor {
                cutoff_ms,
                at_ms: cursor.at_ms,
                id: cursor.id.clone(),
            };
            let json = serde_json::to_string(&stored)
                .map_err(|error| format!("序列化数据保留游标失败: {error}"))?;
            store
                .put_setting(&key, &json)
                .map_err(|error| format!("写入数据保留游标失败: {error}"))
        }
        None => store
            .delete_setting(&key)
            .map(|_| ())
            .map_err(|error| format!("清除数据保留游标失败: {error}")),
    }
}

/// 单类执行结果。
enum ClassOutcome {
    /// 该类窗口已扫完，或 dry_run 的单批检视完成。
    Continue,
    /// 批间停止信号或预算用尽：后续类别不能再开始。
    Stop(String),
}

pub fn run_data_retention(
    state: &ApiState,
    job: &Job,
    policy: &DataRetentionPolicy,
    now: i64,
    check_stop: &dyn Fn() -> MaintenanceStop,
    report: &mut MaintenanceReport,
) -> Result<(), String> {
    report.dry_run = policy.dry_run;
    let started = Instant::now();
    let mut total_batches = 0usize;
    let mut budget_exhausted = false;
    let mut finished_classes = 0usize;

    for (index, class) in DATA_RETENTION_CLASSES.iter().enumerate() {
        let requested_cutoff = cutoff_ms_for_days(now, class_days(policy, *class))?;
        let (active_cutoff, mut cursor) = load_cursor(state, *class, requested_cutoff);
        let mut class_report = DataRetentionClassReport {
            class: class.as_str().to_string(),
            cutoff_ms: active_cutoff,
            examined: 0,
            deleted: 0,
            protected: 0,
            incomplete: false,
            resume_after: cursor.clone(),
        };

        let outcome = loop {
            let stop = check_stop();
            if let Some(note) = stop.stop_note("批") {
                class_report.incomplete = true;
                break ClassOutcome::Stop(note);
            }
            if total_batches >= MAX_BATCHES {
                budget_exhausted = true;
                class_report.incomplete = true;
                break ClassOutcome::Stop(format!(
                    "已达到 {MAX_BATCHES} 批预算，剩余候选留到下一轮"
                ));
            }
            // 单条 SQL 不可中断，所以 2 秒预算只在批间判定（先到先停）。
            if total_batches > 0 && started.elapsed() >= MAX_BATCH_BUDGET {
                budget_exhausted = true;
                class_report.incomplete = true;
                break ClassOutcome::Stop(format!(
                    "已达到 {} 秒预算，剩余候选留到下一轮",
                    MAX_BATCH_BUDGET.as_secs()
                ));
            }

            let batch = {
                let store = state.store.lock();
                store.prune_data_history(
                    *class,
                    active_cutoff,
                    cursor.as_ref(),
                    BATCH_SIZE,
                    policy.dry_run,
                )
            }
            .map_err(|error| error.to_string())?;

            report.examined += batch.examined;
            report.deleted += batch.deleted;
            report.protected += batch.protected;
            report.batches += 1;
            total_batches += 1;
            class_report.examined += batch.examined;
            class_report.deleted += batch.deleted;
            class_report.protected += batch.protected;
            class_report.resume_after = batch.resume_after.clone();

            if policy.dry_run {
                // 试运行每类只检视一批；不能循环重复检视同一批来虚报候选数。
                class_report.incomplete = batch.incomplete;
                break ClassOutcome::Continue;
            }

            cursor = batch.resume_after.clone();
            persist_cursor(state, *class, active_cutoff, cursor.as_ref())?;
            if !batch.incomplete {
                class_report.incomplete = false;
                class_report.resume_after = None;
                break ClassOutcome::Continue;
            }
            class_report.incomplete = true;
        };

        report.classes.push(class_report);
        finished_classes = index + 1;
        if let ClassOutcome::Stop(note) = outcome {
            report.note = Some(note);
            break;
        }
    }

    // 预算/停止导致尚未开始的类别：显式报告 incomplete，而不是假装已经检查过。
    if finished_classes < DATA_RETENTION_CLASSES.len() {
        for class in &DATA_RETENTION_CLASSES[finished_classes..] {
            let cutoff = cutoff_ms_for_days(now, class_days(policy, *class))?;
            report.classes.push(DataRetentionClassReport {
                class: class.as_str().to_string(),
                cutoff_ms: cutoff,
                examined: 0,
                deleted: 0,
                protected: 0,
                incomplete: true,
                resume_after: None,
            });
        }
    }

    report.incomplete = report.classes.iter().any(|entry| entry.incomplete);
    report.remaining = report.incomplete;
    report.budget_exhausted = budget_exhausted;
    report.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    if report.incomplete {
        tracing::warn!(
            job_id = %job.id,
            batches = report.batches,
            examined = report.examined,
            deleted = report.deleted,
            "分类历史保留仍有剩余候选，留到下一轮"
        );
    }
    Ok(())
}
