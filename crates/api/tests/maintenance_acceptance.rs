//! DB8 Task 6：端到端验收。
//!
//! 与 `maintenance_runtime.rs` 的分工：那边验证 `maintenance::run` 的**函数**行为
//! （注入假执行器、直接调用）；这里验证**接线**——真实 `ApiState` 构造（含启动时
//! seed）、真实 `StateRunner` 分派、真实 `SqliteExecutor`、以及重启后的配置保持。

use std::path::Path;
use std::sync::Arc;

#[path = "maintenance_support/mod.rs"]
mod support;

use api::maintenance::{MaintenancePolicy, SqliteExecutor};
use api::worker::StateRunner;
use api::ApiState;
use domain::{MediaId, UserId};
use jobs::{JobKind, Runner, Schedule};
use store::{SessionRow, Store};
use support::{build_state, KEEP, DAY};

/// 真实执行器 + 真实 Store；`ApiState::new` 会在构造时 seed 维护 def。
fn real_state(root: &Path) -> ApiState {
    build_state(
        root,
        Store::open(root.join("data")).unwrap(),
        Arc::new(SqliteExecutor),
    )
}

fn maintenance_defs(state: &ApiState) -> Vec<jobs::JobDef> {
    state
        .jobs()
        .lock()
        .list_defs()
        .unwrap()
        .into_iter()
        .filter(|def| def.kind.is_maintenance())
        .collect()
}

#[test]
fn fresh_state_registers_the_three_maintenance_defs_with_documented_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let state = real_state(tmp.path());
    let defs = maintenance_defs(&state);
    assert_eq!(defs.len(), 3, "启动时应注册三个维护任务: {defs:?}");

    let by_kind = |kind: JobKind| defs.iter().find(|def| def.kind == kind).unwrap();

    let database = by_kind(JobKind::DatabaseMaintenance);
    assert!(database.enabled, "数据库维护默认开启");
    assert_eq!(
        database.schedule,
        Some(Schedule::Interval { secs: 21_600 }),
        "默认每 6 小时"
    );
    assert_eq!(
        MaintenancePolicy::parse(database.kind, &database.payload).unwrap(),
        MaintenancePolicy::Database(api::maintenance::DatabaseMaintenancePolicy {
            version: 1,
            vacuum_enabled: false,
        }),
        "VACUUM 默认关闭"
    );

    let jobs_def = by_kind(JobKind::JobsRetention);
    assert!(jobs_def.enabled, "任务历史清理默认开启");
    assert_eq!(
        jobs_def.schedule,
        Some(Schedule::Interval { secs: 86_400 }),
        "默认每天"
    );

    let playback = by_kind(JobKind::PlaybackHistoryRetention);
    assert!(
        !playback.enabled,
        "播放历史清理默认关闭：删的是用户能看见的历史，不能默认开"
    );
    assert!(
        playback.schedule.is_some(),
        "关闭的任务仍保留非空 schedule，现有定时任务页才看得到"
    );
}

#[test]
fn state_runner_executes_a_jobs_retention_job_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let state = real_state(tmp.path());
    let now = support::unix_now();
    let old = now - 30 * DAY;

    let stale = support::enqueue_claim(&state, JobKind::Scrape, "{}", old);
    state.jobs().lock().succeed_claimed(&stale, old).unwrap();
    let fresh = support::enqueue_claim(&state, JobKind::Scrape, "{}", now);
    state.jobs().lock().succeed_claimed(&fresh, now).unwrap();

    // 真实分派：StateRunner -> maintenance::run -> Queue::prune_terminal_jobs。
    let job = support::enqueue_claim(&state, JobKind::JobsRetention, KEEP, now);
    StateRunner {
        state: state.clone(),
    }
    .run(&job)
    .expect("保留任务应成功");

    let queue = state.jobs();
    let guard = queue.lock();
    assert!(
        guard.get(stale.id).unwrap().is_none(),
        "超过保留期的终止态任务应被删除"
    );
    assert!(
        guard.get(fresh.id).unwrap().is_some(),
        "保留期内的任务必须保留"
    );
    drop(guard);

    let report = support::progress_of(&state, &job);
    assert_eq!(report["kind"], "jobs_retention");
    assert_eq!(report["deleted"], 1, "报告应记录实际删除数: {report}");
    assert_eq!(report["dry_run"], false);
}

#[test]
fn user_changes_survive_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let state = real_state(tmp.path());
    let target = maintenance_defs(&state)
        .into_iter()
        .find(|def| def.kind == JobKind::JobsRetention)
        .unwrap();
    {
        let queue = state.jobs();
        let guard = queue.lock();
        guard.set_def_enabled_by_id(target.id, false).unwrap();
        guard
            .set_def_schedule_by_id(target.id, Some(Schedule::Interval { secs: 7_200 }))
            .unwrap();
    }
    drop(state);

    // 重启：同一 data dir 重新构造 ApiState（会再次 seed）。
    let restarted = real_state(tmp.path());
    let def = maintenance_defs(&restarted)
        .into_iter()
        .find(|def| def.kind == JobKind::JobsRetention)
        .unwrap();
    assert!(!def.enabled, "用户关掉的任务不得被重启后的 seed 重新打开");
    assert_eq!(
        def.schedule,
        Some(Schedule::Interval { secs: 7_200 }),
        "用户改过的频率不得被重置"
    );
    assert_eq!(
        maintenance_defs(&restarted).len(),
        3,
        "重启后 seed 不得产生重复 def"
    );
}

fn session(user: UserId, media: MediaId, device: &str, started: i64) -> SessionRow {
    SessionRow {
        device_id: device.into(),
        user_id: user,
        media_id: media,
        season: None,
        episode: None,
        client: Some("acceptance".into()),
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
        started_at: started,
        last_report_at: started,
        ledger_id: None,
    }
}

#[test]
fn playback_retention_prunes_only_logs_and_keeps_playback_state() {
    let tmp = tempfile::tempdir().unwrap();
    let state = real_state(tmp.path());
    let now = support::unix_now();
    let user = UserId::new();
    let media = MediaId::new();

    {
        let store_handle = state.store();
        let store = store_handle.lock();
        // 两条超龄历史 + 一条近期历史。
        for (index, started) in [(0, now - 40 * DAY), (1, now - 30 * DAY), (2, now - 60)].iter() {
            store
                .upsert_session(&session(user, media, &format!("device-{index}"), *started))
                .unwrap();
            store
                .close_session(user, &format!("device-{index}"), *started + 60)
                .unwrap();
        }
        // 一条活跃会话 + 进度/收藏：清理不得碰它们。
        let mut live = session(user, media, "live-device", now - 30);
        live.position_ms = 12_345;
        store.upsert_session(&live).unwrap();
        store
            .upsert_unit(
                user, media, -1, -1, 42_000, Some(false), Some(true), Some(100_000), None, None,
                false, 100,
            )
            .unwrap();
    }
    let before = {
        let store_handle = state.store();
        let store = store_handle.lock();
        store.unit_state(user, media, -1, -1).unwrap().unwrap()
    };

    let job = support::enqueue_claim(&state, JobKind::PlaybackHistoryRetention, KEEP, now);
    StateRunner {
        state: state.clone(),
    }
    .run(&job)
    .expect("播放历史清理应成功");

    let store_handle = state.store();
    let store = store_handle.lock();
    let scope = store::PlaybackLogScope {
        before: None,
        before_id: None,
        since: None,
        user_id: Some(user),
        visible_media: None,
    };
    let page = store.playback_log_page(&scope, 100).unwrap();
    assert_eq!(
        page.entries.len(),
        1,
        "只剩保留期内的一条历史: {}",
        page.entries.len()
    );
    assert!(
        store.get_session(user, "live-device").unwrap().is_some(),
        "活跃会话不得被清理"
    );
    assert!(
        store.active_sessions(now, 600).unwrap().len() == 1,
        "活跃会话仍可被看到"
    );
    assert_eq!(
        store.unit_state(user, media, -1, -1).unwrap().unwrap(),
        before,
        "播放进度/收藏所在的行不得被清理，也不得被改动"
    );
}

/// 计划 Task 6 要求的大历史批次边界实测。默认 ignored（不拖慢常规 CI）。
///
/// 200_001 行超龄历史 → 单次运行受 `MAX_BATCHES(20) * BATCH_SIZE(500) = 10000`
/// 限制，因此一次只删 10000 行并报告 `budget_exhausted`，剩余交给下一次运行。
/// 这同时证明批次边界不会退化成 `while has_more` 死循环。
#[test]
#[ignore = "manual: 200k 行批次边界与成本实测"]
fn large_history_prune_stops_at_the_budget_and_resumes_next_run() {
    let tmp = tempfile::tempdir().unwrap();
    let state = real_state(tmp.path());
    let now = support::unix_now();
    let user = UserId::new();
    let media = MediaId::new();
    const ROWS: i64 = 200_001;

    // test-only：单事务原始 SQL 造历史（断言仍走公开 seam）。
    {
        let conn = rusqlite::Connection::open(support::data_dir(&state).join("subscribe.db")).unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO playback_logs
                     (id, user_id, media_id, season, episode, client, device_name, play_method,
                      started_at, ended_at, watched_ms, start_position_ms, end_position_ms,
                      duration_ms, completed)
                     VALUES (?1, ?2, ?3, NULL, NULL, NULL, NULL, 'local', ?4, ?4, 10, 0, 500, 1000, 0)",
                )
                .unwrap();
            for index in 0..ROWS {
                let ts = now - 40 * DAY - index;
                stmt.execute(rusqlite::params![
                    format!("big-{index}"),
                    user.to_string(),
                    media.to_string(),
                    ts
                ])
                .unwrap();
            }
        }
        tx.commit().unwrap();
    }

    let first = support::enqueue_claim(&state, JobKind::PlaybackHistoryRetention, KEEP, now);
    let started = std::time::Instant::now();
    StateRunner { state: state.clone() }.run(&first).unwrap();
    let first_elapsed = started.elapsed();
    let report = support::progress_of(&state, &first);
    println!(
        "large-history run#1: batches={} deleted={} budget_exhausted={} elapsed_ms={}",
        report["batches"], report["deleted"], report["budget_exhausted"], first_elapsed.as_millis()
    );
    assert_eq!(report["batches"], 20, "单次运行受 MAX_BATCHES 限制");
    assert_eq!(report["deleted"], 10_000, "单次最多删 MAX_BATCHES*BATCH_SIZE 行");
    assert_eq!(report["budget_exhausted"], true);

    let second = support::enqueue_claim(&state, JobKind::PlaybackHistoryRetention, KEEP, now);
    StateRunner { state: state.clone() }.run(&second).unwrap();
    let report = support::progress_of(&state, &second);
    println!("large-history run#2: deleted={}", report["deleted"]);
    assert_eq!(report["deleted"], 10_000, "下一次运行继续推进，不是死循环");
}
