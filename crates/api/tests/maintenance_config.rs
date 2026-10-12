//! 维护任务的基础合同：默认注册、用户改动不被 seed 覆盖、策略解析边界。
//!
//! 断言全部走公开 API（`Queue` + `api::maintenance`）；payload 的 fixture 写入用
//! 原始 SQL（仓库既有约定：setup 可以用 raw SQL，断言不行）。

use api::maintenance::{
    DatabaseMaintenancePolicy, DEFAULT_DATABASE_INTERVAL_SECS, DEFAULT_JOBS_INTERVAL_SECS,
    DEFAULT_RETENTION_DAYS, JobsRetentionPolicy, MaintenancePolicy, MAX_RETENTION_DAYS,
    PlaybackHistoryRetentionPolicy, POLICY_VERSION, seed_maintenance_defs,
    validate_maintenance_interval,
};
use domain::JobDefId;
use jobs::{JobDef, JobKind, Queue, Schedule};

fn open(tmp: &tempfile::TempDir) -> Queue {
    Queue::open(tmp.path().join("jobs.db")).unwrap()
}

/// test-only fixture：直接改 payload 列，模拟"用户改过保留天数"。
fn overwrite_payload(tmp: &tempfile::TempDir, id: JobDefId, payload: &str) {
    let conn = rusqlite::Connection::open(tmp.path().join("jobs.db")).unwrap();
    conn.execute(
        "UPDATE job_defs SET payload = ?1 WHERE id = ?2",
        rusqlite::params![payload, id.to_string()],
    )
    .unwrap();
}

fn maintenance_def<'a>(defs: &'a [JobDef], kind: JobKind) -> &'a JobDef {
    defs.iter()
        .find(|def| def.kind == kind)
        .unwrap_or_else(|| panic!("缺少维护任务 {:?}", kind))
}

fn retention_days(def: &JobDef) -> u32 {
    match MaintenancePolicy::parse(def.kind, &def.payload).unwrap() {
        MaintenancePolicy::Jobs(policy) => policy.retention_days,
        MaintenancePolicy::PlaybackHistory(policy) => policy.retention_days,
        other => panic!("不是保留类策略: {other:?}"),
    }
}

#[test]
fn seed_creates_three_maintenance_defs_with_documented_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let queue = open(&tmp);
    seed_maintenance_defs(&queue).unwrap();

    let defs = queue.list_defs().unwrap();
    assert_eq!(defs.len(), 3, "只应注册三个维护任务");

    let database = maintenance_def(&defs, JobKind::DatabaseMaintenance);
    assert!(database.enabled, "数据库维护默认开启");
    assert_eq!(
        database.schedule,
        Some(Schedule::Interval {
            secs: DEFAULT_DATABASE_INTERVAL_SECS
        })
    );
    assert_eq!(
        MaintenancePolicy::parse(database.kind, &database.payload).unwrap(),
        MaintenancePolicy::Database(DatabaseMaintenancePolicy {
            version: POLICY_VERSION,
            vacuum_enabled: false,
        }),
        "VACUUM 子功能默认关闭"
    );

    let jobs = maintenance_def(&defs, JobKind::JobsRetention);
    assert!(jobs.enabled, "任务历史清理默认开启");
    assert_eq!(
        jobs.schedule,
        Some(Schedule::Interval {
            secs: DEFAULT_JOBS_INTERVAL_SECS
        })
    );
    assert_eq!(
        MaintenancePolicy::parse(jobs.kind, &jobs.payload).unwrap(),
        MaintenancePolicy::Jobs(JobsRetentionPolicy {
            version: POLICY_VERSION,
            retention_days: DEFAULT_RETENTION_DAYS,
            dry_run: false,
        })
    );

    let playback = maintenance_def(&defs, JobKind::PlaybackHistoryRetention);
    assert!(!playback.enabled, "播放历史清理默认关闭");
    assert!(
        playback.schedule.is_some(),
        "关闭的任务仍保留非空 schedule，现有页面才看得到它"
    );
    assert_eq!(
        MaintenancePolicy::parse(playback.kind, &playback.payload).unwrap(),
        MaintenancePolicy::PlaybackHistory(PlaybackHistoryRetentionPolicy {
            version: POLICY_VERSION,
            retention_days: DEFAULT_RETENTION_DAYS,
            dry_run: false,
        })
    );
}

#[test]
fn seed_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let queue = open(&tmp);
    seed_maintenance_defs(&queue).unwrap();
    seed_maintenance_defs(&queue).unwrap();
    assert_eq!(queue.list_defs().unwrap().len(), 3, "重复 seed 不得产生重复 def");
}

#[test]
fn reseed_after_restart_keeps_user_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let queue = open(&tmp);
    seed_maintenance_defs(&queue).unwrap();
    let jobs_def_id = maintenance_def(&queue.list_defs().unwrap(), JobKind::JobsRetention).id;

    queue.set_def_enabled_by_id(jobs_def_id, false).unwrap();
    queue
        .set_def_schedule_by_id(jobs_def_id, Some(Schedule::Interval { secs: 7200 }))
        .unwrap();
    overwrite_payload(
        &tmp,
        jobs_def_id,
        r#"{"version":1,"retention_days":7,"dry_run":true}"#,
    );
    drop(queue);

    // 重启后再次 seed：用户改动必须原样保留。
    let queue = open(&tmp);
    seed_maintenance_defs(&queue).unwrap();
    let defs = queue.list_defs().unwrap();
    let jobs = maintenance_def(&defs, JobKind::JobsRetention);
    assert!(!jobs.enabled, "用户关掉的任务不得被 seed 重新打开");
    assert_eq!(
        jobs.schedule,
        Some(Schedule::Interval { secs: 7200 }),
        "用户改过的频率不得被重置"
    );
    assert_eq!(retention_days(jobs), 7, "用户改过的保留天数不得被重置");
}

#[test]
fn accepts_boundary_retention_days() {
    for days in [1_u32, MAX_RETENTION_DAYS] {
        let payload = format!(r#"{{"version":1,"retention_days":{days},"dry_run":false}}"#);
        let parsed = MaintenancePolicy::parse(JobKind::JobsRetention, &payload).unwrap();
        assert_eq!(
            parsed,
            MaintenancePolicy::Jobs(JobsRetentionPolicy {
                version: POLICY_VERSION,
                retention_days: days,
                dry_run: false,
            })
        );
    }
}

#[test]
fn rejects_out_of_range_or_malformed_retention() {
    let rejected = [
        r#"{"version":1,"retention_days":0,"dry_run":false}"#,
        r#"{"version":1,"retention_days":36501,"dry_run":false}"#,
        r#"{"version":1,"retention_days":-1,"dry_run":false}"#,
        r#"{"version":1,"retention_days":99999999999999999999,"dry_run":false}"#,
        r#"{"version":1,"retention_days":90}"#,
        r#"{"version":1,"retention_days":90,"dry_run":false,"extra":1}"#,
        r#"{"version":2,"retention_days":90,"dry_run":false}"#,
        r#"{"retention_days":90,"dry_run":false}"#,
        r#"{"version":1,"retention_days":90,"dry_run":"yes"}"#,
        r#"not json"#,
        "",
    ];
    for payload in rejected {
        assert!(
            MaintenancePolicy::parse(JobKind::JobsRetention, payload).is_err(),
            "必须拒绝: {payload}"
        );
    }
}

#[test]
fn rejects_non_maintenance_kinds_and_bad_database_policy() {
    assert!(
        MaintenancePolicy::parse(JobKind::Transfer, r#"{"version":1}"#).is_err(),
        "非维护任务不得解析成维护策略"
    );
    assert!(
        MaintenancePolicy::parse(
            JobKind::DatabaseMaintenance,
            r#"{"version":1,"vacuum_enabled":true}"#
        )
        .is_ok(),
        "显式开启 VACUUM 是合法配置"
    );
    for payload in [
        r#"{"version":1}"#,
        r#"{"version":1,"vacuum_enabled":"true"}"#,
        r#"{"version":1,"vacuum_enabled":false,"extra":1}"#,
    ] {
        assert!(
            MaintenancePolicy::parse(JobKind::DatabaseMaintenance, payload).is_err(),
            "必须拒绝: {payload}"
        );
    }
}

#[test]
fn maintenance_interval_bounds_are_enforced() {
    assert!(validate_maintenance_interval(3600).is_ok());
    assert!(validate_maintenance_interval(31536000).is_ok());
    for bad in [0_u64, 3599, 31536001, u64::MAX] {
        assert!(
            validate_maintenance_interval(bad).is_err(),
            "必须拒绝 interval={bad}"
        );
    }
}
