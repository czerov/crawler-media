//! DB8 Task 4：维护任务 runner 的编排行为（锁、避让、allowlist、保留批次、tick）。
//!
//! 断言走公开 seam（`maintenance::run`、`Queue`、`Store`、HTTP `/jobs/tick`）。
//! 真实 SQLite 执行器见 `maintenance_sqlite.rs`。

use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex, mpsc};
use std::time::Duration;

use api::maintenance::{
    DatabaseExecutor, DatabaseName, DatabaseOutcome, DatabaseReport, DatabaseTarget,
    MaintenanceContext, SqliteExecutor, database_targets, run,
};
use api::worker::StateRunner;
use api::{ApiState, Store};
use domain::MediaKind;
use jobs::{Job, JobKind, NewDef, Runner};
use parking_lot::Mutex;
use serde_json::{Value, json};

#[path = "maintenance_support/mod.rs"]
mod support;
use support::{
    DAY, DB_LIGHT, DB_VACUUM, KEEP, PREVIEW, Recorder, TOKEN, build_state, data_dir, db_entry,
    enqueue_claim, progress_of, temp_state, unix_now,
};

/// 第一次进入 `execute` 后阻塞，直到测试放行；用来证明维护不在 Store/Queue 锁内执行。
struct BlockingExecutor {
    entered: mpsc::Sender<()>,
    release: StdMutex<mpsc::Receiver<()>>,
    blocked: std::sync::atomic::AtomicBool,
}

impl DatabaseExecutor for BlockingExecutor {
    fn execute(
        &self,
        target: &DatabaseTarget,
        _vacuum_enabled: bool,
        _context: &MaintenanceContext<'_>,
    ) -> Result<DatabaseReport, String> {
        if !self.blocked.swap(true, std::sync::atomic::Ordering::SeqCst) {
            self.entered.send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
        }
        Ok(DatabaseReport {
            name: target.name.as_str().to_string(),
            outcome: DatabaseOutcome::Optimized,
            free_pages: 0,
            page_size: 4096,
            size_before: 0,
            size_after: 0,
            skip_reason: None,
            elapsed_ms: 1,
        })
    }
}

/// 只让 app 库打不开的假执行器：验证"其余库跑完再汇总失败"。
struct AppOpenFailure {
    attempted: Mutex<Vec<DatabaseName>>,
}

impl DatabaseExecutor for AppOpenFailure {
    fn execute(
        &self,
        target: &DatabaseTarget,
        _vacuum_enabled: bool,
        _context: &MaintenanceContext<'_>,
    ) -> Result<DatabaseReport, String> {
        self.attempted.lock().push(target.name);
        if target.name == DatabaseName::App {
            return Err("打开数据库 app.db 失败: fixture".into());
        }
        Ok(DatabaseReport {
            name: target.name.as_str().to_string(),
            outcome: DatabaseOutcome::Optimized,
            free_pages: 0,
            page_size: 4096,
            size_before: 0,
            size_after: 0,
            skip_reason: None,
            elapsed_ms: 1,
        })
    }
}

#[test]
fn one_failed_database_fails_the_job_after_running_the_rest() {
    let executor = Arc::new(AppOpenFailure {
        attempted: Mutex::new(Vec::new()),
    });
    let (_tmp, state) = temp_state(executor.clone());
    let job = enqueue_claim(&state, JobKind::DatabaseMaintenance, DB_LIGHT, unix_now());

    let error = run(&state, &job).expect_err("任一库 Failed 必须让整个 job 失败");

    assert!(error.contains("app"), "错误必须保留库名与上下文: {error}");
    assert!(
        executor.attempted.lock().len() > 1,
        "其余库必须继续跑完再汇总: {:?}",
        executor.attempted.lock()
    );
    let report = progress_of(&state, &job);
    assert_eq!(db_entry(&report, "app")["outcome"], "failed");
    assert_eq!(db_entry(&report, "jobs")["outcome"], "optimized");
}

#[test]
fn maintenance_runs_outside_store_and_queue_locks() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (tmp, state) = temp_state(Arc::new(BlockingExecutor {
        entered: entered_tx,
        release: StdMutex::new(release_rx),
        blocked: std::sync::atomic::AtomicBool::new(false),
    }));
    assert!(tmp.path().exists());
    let job = enqueue_claim(&state, JobKind::DatabaseMaintenance, DB_LIGHT, unix_now());

    let run_state = state.clone();
    let handle = std::thread::spawn(move || run(&run_state, &job));
    entered_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("执行器必须被调用");

    assert!(
        state.store().try_lock().is_some(),
        "维护执行期间不得持有 Store 锁"
    );
    assert!(
        state.jobs().try_lock().is_some(),
        "维护执行期间不得持有 Queue 锁"
    );

    release_tx.send(()).unwrap();
    handle.join().unwrap().unwrap();
}

#[test]
fn active_library_work_downgrades_vacuum_but_still_optimizes() {
    let recorder = Arc::new(Recorder::default());
    let (tmp, state) = temp_state(recorder.clone());
    let root = tmp.path().join("media");
    std::fs::create_dir_all(&root).unwrap();
    let library = state
        .store()
        .lock()
        .create_library(
            MediaKind::Tv,
            "Extra",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let lease = state
        .library_work()
        .acquire(&state.store(), &library.id)
        .unwrap();
    assert!(state.library_work().has_active_work());

    let job = enqueue_claim(&state, JobKind::DatabaseMaintenance, DB_VACUUM, unix_now());
    run(&state, &job).unwrap();

    assert!(
        recorder.seen().iter().all(|(_, _, vacuum)| !vacuum),
        "Library 工作运行时必须跳过重型维护"
    );
    let report = progress_of(&state, &job);
    let note = report["note"].as_str().unwrap_or_default();
    assert!(
        note.contains("Library"),
        "报告必须写明跳过重型维护的原因: {report}"
    );
    drop(lease);
    assert!(!state.library_work().has_active_work());
}

#[test]
fn missing_optional_fanart_is_skipped_and_never_created() {
    let (tmp, state) = temp_state(Arc::new(SqliteExecutor));
    let data = data_dir(&state);
    let fanart = data.join("fanart.db");
    assert!(!fanart.exists());

    let job = enqueue_claim(&state, JobKind::DatabaseMaintenance, DB_LIGHT, unix_now());
    run(&state, &job).unwrap();

    let report = progress_of(&state, &job);
    let entry = db_entry(&report, "fanart");
    assert_eq!(entry["outcome"], "skipped");
    assert!(
        entry["skip_reason"]
            .as_str()
            .unwrap_or_default()
            .contains("不存在"),
        "跳过原因必须写明文件不存在: {entry}"
    );
    assert!(!fanart.exists(), "缺失的可选库不得被创建");
    assert!(tmp.path().exists());
}

#[test]
fn custom_app_path_is_used_instead_of_data_dir_app_db() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let custom = tmp.path().join("custom").join("app.db");
    let store = Store::open_at(&data, &custom).unwrap();
    let recorder = Arc::new(Recorder::default());
    let state = build_state(tmp.path(), store, recorder.clone());

    let job = enqueue_claim(&state, JobKind::DatabaseMaintenance, DB_LIGHT, unix_now());
    run(&state, &job).unwrap();

    let seen = recorder.seen();
    assert!(
        seen.iter()
            .any(|(name, path, _)| *name == DatabaseName::App && *path == custom),
        "App 目标必须用真实 app_path: {seen:?}"
    );
    assert!(
        !seen.iter().any(|(_, path, _)| *path == data.join("app.db")),
        "不得假定 data_dir/app.db: {seen:?}"
    );
}

#[test]
fn database_targets_are_an_allowlist_with_optional_fanart() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let app = tmp.path().join("custom").join("app.db");

    let targets = database_targets(&data, &app);
    let names: Vec<&str> = targets.iter().map(|target| target.name.as_str()).collect();
    assert_eq!(names, ["app", "catalog", "library", "subscribe", "jobs"]);

    std::fs::write(data.join("fanart.db"), b"fixture").unwrap();
    let targets = database_targets(&data, &app);
    let names: Vec<&str> = targets.iter().map(|target| target.name.as_str()).collect();
    assert_eq!(
        names,
        ["app", "catalog", "library", "subscribe", "jobs", "fanart"]
    );
    assert!(targets.iter().all(|target| target.name.as_str() != ".."));
}

// ---------------------------------------------------------------------------
// 保留批次
// ---------------------------------------------------------------------------

/// test-only：单事务原始 SQL 批量造终止态历史（断言仍走公开 seam）。
fn raw_terminal_jobs(data: &Path, def_id: Option<&str>, count: usize, finished_at: i64) {
    let conn = rusqlite::Connection::open(data.join("jobs.db")).unwrap();
    conn.execute_batch("BEGIN").unwrap();
    {
        let mut stmt = conn
            .prepare(
                "INSERT INTO jobs
                    (id, def_id, kind, payload, status, attempt, run_after, started_at,
                     finished_at, error, progress, concurrency_key, timeout_secs)
                 VALUES (?1, ?2, 'jobs_retention', '{}', 'succeeded', 0, ?3, ?3, ?3, NULL, NULL, NULL, NULL)",
            )
            .unwrap();
        for index in 0..count {
            stmt.execute(rusqlite::params![
                format!("fixture-{index}"),
                def_id,
                finished_at
            ])
            .unwrap();
        }
    }
    conn.execute_batch("COMMIT").unwrap();
}

/// 只数 fixture 造出来的历史，避免把实例 def 的取消行算进来。
fn terminal_count(data: &Path) -> i64 {
    let conn = rusqlite::Connection::open(data.join("jobs.db")).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM jobs
         WHERE id LIKE 'fixture-%' AND status IN ('succeeded','failed','cancelled')",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn jobs_retention_stops_at_max_batches_and_reports_budget() {
    let (_tmp, state) = temp_state(Arc::new(SqliteExecutor));
    let data = data_dir(&state);
    let now = unix_now();
    raw_terminal_jobs(&data, None, 10_100, now - 10 * DAY);

    let job = enqueue_claim(&state, JobKind::JobsRetention, KEEP, now);
    run(&state, &job).unwrap();

    let report = progress_of(&state, &job);
    assert_eq!(report["batches"], 20, "单次最多 20 批: {report}");
    assert_eq!(report["deleted"], 10_000, "20 × 500 = 10000: {report}");
    assert_eq!(report["budget_exhausted"], true);
    assert_eq!(report["kind"], "jobs_retention");
    assert_eq!(terminal_count(&data), 100, "预算耗尽后剩余候选留到下一轮");
}

#[test]
fn jobs_retention_does_not_loop_when_all_candidates_are_protected() {
    let (_tmp, state) = temp_state(Arc::new(SqliteExecutor));
    let data = data_dir(&state);
    let now = unix_now();
    let def = state
        .jobs()
        .lock()
        .upsert_def(NewDef {
            kind: JobKind::JobsRetention,
            name: "tied".into(),
            enabled: false,
            schedule: None,
            payload: "{}".into(),
            timeout_secs: None,
            concurrency_key: Some("tied".into()),
        })
        .unwrap();
    // 完全并列的 last-child 组：整组保留。修复 C1 后它们**不进候选窗口**，因此
    // has_more=false、不会空转；protected 作为诊断计数上限为 batch_size+1。
    raw_terminal_jobs(&data, Some(&def.id.to_string()), 600, now - 10 * DAY);

    let job = enqueue_claim(&state, JobKind::JobsRetention, KEEP, now);
    run(&state, &job).unwrap();

    let report = progress_of(&state, &job);
    assert_eq!(report["batches"], 1, "没有可删候选时只跑一批: {report}");
    assert_eq!(report["deleted"], 0);
    assert_eq!(
        report["candidates"], 0,
        "并列 last-child 组不进候选窗口: {report}"
    );
    assert_eq!(report["protected"], 500, "诊断计数上限 batch_size (500)");
    assert_eq!(
        report["protected_is_lower_bound"], true,
        "超过 batch_size 探测上限时必须标记为下界: {report}"
    );
    assert_eq!(
        report["budget_exhausted"], false,
        "没有更多可删候选，不是预算耗尽: {report}"
    );
    assert_eq!(terminal_count(&data), 600, "并列 last-child 组必须全部保留");
}

#[test]
fn cancelled_retention_does_not_start_another_batch() {
    let (_tmp, state) = temp_state(Arc::new(SqliteExecutor));
    let data = data_dir(&state);
    let now = unix_now();
    raw_terminal_jobs(&data, None, 1_200, now - 10 * DAY);

    let job = enqueue_claim(&state, JobKind::JobsRetention, KEEP, now);
    // 运行中被取消：任务行不再是本次 running attempt。
    let conn = rusqlite::Connection::open(data.join("jobs.db")).unwrap();
    conn.execute(
        "UPDATE jobs SET status = 'cancelled', finished_at = ?2 WHERE id = ?1",
        rusqlite::params![job.id.to_string(), now],
    )
    .unwrap();

    run(&state, &job).unwrap();

    let report = progress_of(&state, &job);
    assert_eq!(report["batches"], 0, "取消后不得启动批次: {report}");
    assert_eq!(report["deleted"], 0);
    assert_eq!(terminal_count(&data), 1_200, "已提交批次之外的候选不动");
}

#[test]
fn playback_retention_dry_run_keeps_logs_then_prunes_only_logs() {
    use domain::{MediaId, UserId};
    use store::SessionRow;

    let (_tmp, state) = temp_state(Arc::new(SqliteExecutor));
    let user = UserId::new();
    let media = MediaId::new();
    let now = unix_now();
    let cutoff = now - DAY;
    let store = state.store();
    {
        let guard = store.lock();
        guard
            .upsert_unit(
                user,
                media,
                -1,
                -1,
                42_000,
                Some(false),
                Some(true),
                Some(100_000),
                None,
                None,
                false,
                100,
            )
            .unwrap();
        for index in 0..2 {
            let session = SessionRow {
                device_id: format!("old{index}"),
                user_id: user,
                media_id: media,
                season: None,
                episode: None,
                client: Some("fixture".into()),
                device_name: None,
                client_version: None,
                play_method: "local".into(),
                position_ms: 500,
                start_position_ms: 0,
                duration_ms: Some(1000),
                paused: false,
                watched_ms: 10,
                rate_bps: 0,
                bytes_sent: 0,
                connections: 0,
                admin_ended: false,
                started_at: cutoff - 10 * DAY,
                last_report_at: cutoff - 10 * DAY,
                ledger_id: None,
            };
            guard.upsert_session(&session).unwrap();
            guard
                .close_session(user, &session.device_id, cutoff - 5 * DAY - index)
                .unwrap()
                .unwrap();
        }
    }
    let progress_before = store.lock().unit_state(user, media, -1, -1).unwrap();

    let preview = enqueue_claim(&state, JobKind::PlaybackHistoryRetention, PREVIEW, now);
    run(&state, &preview).unwrap();
    let report = progress_of(&state, &preview);
    assert_eq!(report["deleted"], 0, "dry_run 必须零删除: {report}");
    assert_eq!(
        store
            .lock()
            .list_logs(100, None, None, None, 0, Some(user))
            .unwrap()
            .len(),
        2
    );

    let real = enqueue_claim(&state, JobKind::PlaybackHistoryRetention, KEEP, now);
    run(&state, &real).unwrap();
    let report = progress_of(&state, &real);
    assert_eq!(report["deleted"], 2);
    assert!(
        store
            .lock()
            .list_logs(100, None, None, None, 0, Some(user))
            .unwrap()
            .is_empty(),
        "只清理保留期外的播放历史"
    );
    assert_eq!(
        store.lock().unit_state(user, media, -1, -1).unwrap(),
        progress_before,
        "播放进度不得被清理"
    );
}

// ---------------------------------------------------------------------------
// 报告、/jobs/tick 与 Queue::tick
// ---------------------------------------------------------------------------

#[test]
fn run_writes_a_complete_report_into_job_progress() {
    let (_tmp, state) = temp_state(Arc::new(SqliteExecutor));
    let now = unix_now();
    let job = enqueue_claim(&state, JobKind::JobsRetention, KEEP, now);
    run(&state, &job).unwrap();

    let report = progress_of(&state, &job);
    for field in [
        "kind",
        "policy_version",
        "cutoff",
        "dry_run",
        "scanned",
        "candidates",
        "deleted",
        "protected",
        "batches",
        "budget_exhausted",
        "elapsed_ms",
    ] {
        assert!(
            report.get(field).is_some(),
            "报告缺少字段 {field}: {report}"
        );
    }
    assert_eq!(report["cutoff"], now - DAY);
    assert_eq!(report["dry_run"], false);
    assert_eq!(report["policy_version"], 1);

    let db = enqueue_claim(&state, JobKind::DatabaseMaintenance, DB_LIGHT, now);
    run(&state, &db).unwrap();
    let report = progress_of(&state, &db);
    assert!(
        report["databases"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()),
        "数据库维护报告必须含每库报告数组: {report}"
    );
    assert_eq!(report["cutoff"], Value::Null, "数据库维护没有保留窗口");
}

#[test]
fn malformed_policy_is_refused_without_falling_back_to_deletion() {
    let (_tmp, state) = temp_state(Arc::new(SqliteExecutor));
    let data = data_dir(&state);
    let now = unix_now();
    raw_terminal_jobs(&data, None, 3, now - 10 * DAY);
    let job = enqueue_claim(
        &state,
        JobKind::JobsRetention,
        r#"{"version":1,"retention_days":0,"dry_run":false}"#,
        now,
    );

    let error = run(&state, &job).expect_err("非法策略必须直接失败");

    assert!(error.contains("保留天数"), "错误必须说明原因: {error}");
    assert_eq!(terminal_count(&data), 3, "解析失败不得回退成更激进的删除");
}

#[tokio::test]
async fn jobs_tick_http_never_executes_maintenance_and_says_so() {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let (_tmp, state) = temp_state(Arc::new(SqliteExecutor));
    // 只留数据库维护一个 def，确保这轮 tick 的维护任务确实到期。
    {
        let queue = state.jobs();
        let guard = queue.lock();
        for def in guard.list_defs().unwrap() {
            guard
                .set_def_enabled_by_id(def.id, def.kind == JobKind::DatabaseMaintenance)
                .unwrap();
        }
    }
    let app = api::router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/jobs/tick")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {TOKEN}"))
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["data"]["ran"], 0, "开发入口不得执行维护任务: {body}");
    assert_eq!(
        body["data"]["skipped_maintenance"],
        json!(["database_maintenance"]),
        "必须显式报告被跳过的维护任务: {body}"
    );
    // 维护确实没有执行：既没有写入冷却时间，也没有留下 running 子任务。
    let store = state.store();
    assert_eq!(
        store
            .lock()
            .get_setting("maintenance.vacuum_last_success.app")
            .unwrap(),
        None
    );
    let queue = state.jobs();
    let guard = queue.lock();
    let def = guard
        .list_defs()
        .unwrap()
        .into_iter()
        .find(|def| def.kind == JobKind::DatabaseMaintenance)
        .unwrap();
    let live = guard.get_live_child(def.id).unwrap();
    assert!(
        live.as_ref()
            .is_none_or(|job| job.status != jobs::JobStatus::Running),
        "开发入口不得留下 running 维护任务: {live:?}"
    );
}

#[test]
fn queue_tick_path_refuses_maintenance_instead_of_deadlocking() {
    let (_tmp, state) = temp_state(Arc::new(SqliteExecutor));
    {
        let queue = state.jobs();
        let guard = queue.lock();
        for def in guard.list_defs().unwrap() {
            guard
                .set_def_enabled_by_id(def.id, def.kind == JobKind::DatabaseMaintenance)
                .unwrap();
        }
    }
    let now = unix_now();
    let (tx, rx) = mpsc::channel();
    let tick_state = state.clone();
    std::thread::spawn(move || {
        let queue = tick_state.jobs();
        let guard = queue.lock();
        let outcome = guard.tick(
            now,
            &StateRunner {
                state: tick_state.clone(),
            },
        );
        let _ = tx.send(outcome.map(|jobs| {
            jobs.into_iter()
                .map(|job| (job.status, job.error))
                .collect::<Vec<_>>()
        }));
    });

    let finished = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("Queue::tick 不得在维护任务上死锁");
    let finished = finished.unwrap();
    assert_eq!(finished.len(), 1, "只应领取维护任务: {finished:?}");
    let error = finished[0].1.clone().unwrap_or_default();
    assert!(
        error.contains("Queue 锁"),
        "必须给出明确拒绝原因而不是静默执行: {finished:?}"
    );
    assert!(json!({ "ok": true }).is_object());
}

/// DB8 复核 I1 回归：试运行不能把 `has_more` 吞掉说"已完成"。
///
/// dry_run 不删除，因此"再取下一批"只会重复检视同一批行（虚报候选数），所以只检视
/// 一批是正确的；但必须把 `has_more` 如实映射成 `budget_exhausted`，否则用户开 dry_run
/// 想看规模，却得到"只有 500 条、已完成"的结论。
#[test]
fn dry_run_reports_a_truncated_budget_instead_of_claiming_completion() {
    let (_tmp, state) = temp_state(Arc::new(SqliteExecutor));
    let data = data_dir(&state);
    let now = unix_now();
    // 1200 条可删候选（def_id 为 NULL → 不受 last-child 保护）。
    raw_terminal_jobs(&data, None, 1_200, now - 40 * DAY);

    let job = enqueue_claim(&state, JobKind::JobsRetention, PREVIEW, now);
    run(&state, &job).unwrap();

    let report = progress_of(&state, &job);
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["deleted"], 0, "dry_run 零删除: {report}");
    assert_eq!(report["candidates"], 500, "试运行检视一批: {report}");
    assert_eq!(
        report["budget_exhausted"], true,
        "还有更多可删候选时必须如实报告，不能说'已完成': {report}"
    );
    assert_eq!(terminal_count(&data), 1_200, "dry_run 不写库");
}
