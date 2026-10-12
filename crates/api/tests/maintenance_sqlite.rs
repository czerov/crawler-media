//! DB8 Task 4：真实 SQLite 执行器的行为（optimize / VACUUM 准入 / busy / 中断）。
//!
//! 全部用临时目录里的真实 SQLite 文件，不碰 live 数据。

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use api::maintenance::{
    ConfiguredSqliteExecutor, DatabaseExecutor, DatabaseName, DatabaseOutcome, DatabaseTarget,
    MaintenanceContext, SqliteExecutor, SqliteLimits, database_targets, run,
};
use jobs::JobKind;

#[path = "maintenance_support/mod.rs"]
mod support;
use support::{
    DB_LIGHT, DB_VACUUM, build_state, data_dir, db_entry, enqueue_claim, progress_of, temp_state,
    unix_now,
};

// ---------------------------------------------------------------------------
// 真实 SQLite 执行器
// ---------------------------------------------------------------------------

fn tiny_limits(per_database_limit: Duration) -> SqliteLimits {
    SqliteLimits {
        min_free_bytes: 0,
        min_free_ratio_percent: 0,
        vacuum_headroom_bytes: 0,
        per_database_limit,
    }
}

fn app_target(data: &Path, app: &Path) -> DatabaseTarget {
    database_targets(data, app)
        .into_iter()
        .find(|target| target.name == DatabaseName::App)
        .expect("allowlist 必须包含 app")
}

fn never_cancelled() -> impl Fn() -> bool {
    || false
}

/// 约 32MiB 的临时库：VACUUM 需要真实的重写时间，中断才有可观察的窗口。
fn seed_large_db(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE payload (id INTEGER PRIMARY KEY, body TEXT);
         WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 8000)
         INSERT INTO payload (id, body) SELECT x, hex(zeroblob(2000)) FROM c;
         DELETE FROM payload WHERE id % 2 = 0;",
    )
    .unwrap();
}

#[test]
fn optimize_keeps_business_data_and_default_never_vacuums() {
    let (_tmp, state) = temp_state(Arc::new(SqliteExecutor));
    state
        .store()
        .lock()
        .put_setting("maintenance.keep", "kept")
        .unwrap();
    let conn = rusqlite::Connection::open(data_dir(&state).join("app.db")).unwrap();
    conn.execute_batch("CREATE TABLE business (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    conn.execute("INSERT INTO business (name) VALUES ('row')", [])
        .unwrap();
    drop(conn);

    let job = enqueue_claim(&state, JobKind::DatabaseMaintenance, DB_LIGHT, unix_now());
    run(&state, &job).unwrap();

    let report = progress_of(&state, &job);
    let outcomes: Vec<&str> = report["databases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["outcome"].as_str().unwrap())
        .collect();
    assert!(
        outcomes
            .iter()
            .all(|outcome| *outcome == "optimized" || *outcome == "skipped"),
        "默认不 VACUUM: {report}"
    );
    assert!(
        !outcomes.contains(&"vacuumed"),
        "vacuum_enabled=false 时不得出现 vacuumed: {report}"
    );
    let store = state.store();
    assert_eq!(
        store
            .lock()
            .get_setting("maintenance.keep")
            .unwrap()
            .as_deref(),
        Some("kept")
    );
    assert_eq!(
        store
            .lock()
            .get_setting("maintenance.vacuum_last_success.app")
            .unwrap(),
        None,
        "没有 VACUUM 成功就不得推进冷却时间"
    );
    let conn = rusqlite::Connection::open(data_dir(&state).join("app.db")).unwrap();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM business", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 1, "optimize 不得改动业务数据");
}

#[test]
fn vacuum_runs_only_when_enabled_and_cooldown_allows() {
    let executor = Arc::new(ConfiguredSqliteExecutor::with_limits(tiny_limits(
        Duration::from_secs(30),
    )));
    let (_tmp, state) = temp_state(executor);
    let now = unix_now();

    let job = enqueue_claim(&state, JobKind::DatabaseMaintenance, DB_VACUUM, now);
    run(&state, &job).unwrap();
    let report = progress_of(&state, &job);
    assert_eq!(
        db_entry(&report, "app")["outcome"],
        "vacuumed",
        "显式开启且满足阈值/冷却时必须真的 VACUUM: {report}"
    );
    let recorded = state
        .store()
        .lock()
        .get_setting("maintenance.vacuum_last_success.app")
        .unwrap();
    assert!(recorded.is_some(), "VACUUM 成功后必须记录成功时间");

    let second = enqueue_claim(&state, JobKind::DatabaseMaintenance, DB_VACUUM, now + 10);
    run(&state, &second).unwrap();
    let report = progress_of(&state, &second);
    let app = db_entry(&report, "app");
    assert_eq!(app["outcome"], "skipped");
    assert!(
        app["skip_reason"]
            .as_str()
            .unwrap_or_default()
            .contains("冷却"),
        "必须写明是冷却期未到: {app}"
    );
}

#[test]
fn busy_database_is_skipped_not_failed() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let app = data.join("app.db");
    seed_large_db(&app);

    let executor = ConfiguredSqliteExecutor::with_limits(tiny_limits(Duration::from_secs(30)));
    let blocker = rusqlite::Connection::open(&app).unwrap();
    blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();

    let cancelled = never_cancelled();
    let context = MaintenanceContext {
        now: unix_now(),
        last_vacuum_success: None,
        cancelled: &cancelled,
    };
    let report = executor
        .execute(&app_target(&data, &app), true, &context)
        .unwrap();

    assert_eq!(report.outcome, DatabaseOutcome::Skipped);
    assert!(report.skip_reason.is_some(), "跳过必须带原因: {report:?}");
    blocker.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn insufficient_disk_headroom_skips_vacuum() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let app = data.join("app.db");
    seed_large_db(&app);

    let executor = ConfiguredSqliteExecutor::with_limits(SqliteLimits {
        vacuum_headroom_bytes: u64::MAX,
        ..tiny_limits(Duration::from_secs(30))
    });
    let cancelled = never_cancelled();
    let context = MaintenanceContext {
        now: unix_now(),
        last_vacuum_success: None,
        cancelled: &cancelled,
    };
    let report = executor
        .execute(&app_target(&data, &app), true, &context)
        .unwrap();

    assert_eq!(report.outcome, DatabaseOutcome::Skipped);
    assert!(
        report
            .skip_reason
            .as_deref()
            .unwrap_or_default()
            .contains("磁盘"),
        "必须写明磁盘不足: {report:?}"
    );
}

/// 超时中断：看门狗在 `per_database_limit` 到期后 interrupt，该库记 `Failed`；
/// 反复执行后进程内不得残留看门狗线程。
///
/// 上限取 1ms：到期落在 VACUUM 的**可中断**阶段（拷贝前的内部语句）。SQLite 的页
/// 拷贝阶段不检查 interrupt 标志，若上限到期落在那里就无法停止（见报告"偏离"）。
#[test]
fn interrupted_vacuum_fails_bounded_and_leaves_no_watchdog_thread() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let app = data.join("app.db");
    seed_large_db(&app);

    let executor = ConfiguredSqliteExecutor::with_limits(tiny_limits(Duration::from_millis(1)));
    let target = app_target(&data, &app);
    let cancelled = never_cancelled();
    let context = MaintenanceContext {
        now: unix_now(),
        last_vacuum_success: None,
        cancelled: &cancelled,
    };

    let before = live_threads();
    for _ in 0..20 {
        let report = executor.execute(&target, true, &context).unwrap();
        assert_eq!(
            report.outcome,
            DatabaseOutcome::Failed,
            "超时必须中断该库: {report:?}"
        );
        assert!(
            report
                .skip_reason
                .as_deref()
                .unwrap_or_default()
                .contains("中断"),
            "错误必须体现超时中断: {report:?}"
        );
    }
    if let (Some(before), Some(after)) = (before, live_threads()) {
        assert!(
            after <= before + 5,
            "看门狗线程必须被 join，不得残留: before={before} after={after}"
        );
    }
}

/// 进程内线程数（用于证明看门狗被 join）；不支持的平台返回 None 并跳过断言。

#[cfg(target_os = "linux")]
fn live_threads() -> Option<usize> {
    Some(std::fs::read_dir("/proc/self/task").ok()?.count())
}

#[cfg(target_os = "macos")]
fn live_threads() -> Option<usize> {
    // SAFETY: 只读取本进程的线程列表；按 Mach 约定用 vm_deallocate 释放内核返回的数组。
    unsafe {
        let mut list: libc::thread_act_array_t = std::ptr::null_mut();
        let mut count: libc::mach_msg_type_number_t = 0;
        if libc::task_threads(libc::mach_task_self(), &mut list, &mut count) != libc::KERN_SUCCESS {
            return None;
        }
        let bytes = (count as usize).saturating_mul(std::mem::size_of::<libc::thread_act_t>());
        libc::vm_deallocate(libc::mach_task_self(), list as libc::vm_address_t, bytes);
        Some(count as usize)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn live_threads() -> Option<usize> {
    None
}
