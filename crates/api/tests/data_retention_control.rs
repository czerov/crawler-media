//! DB-05 控制面：默认关闭/dry_run、禁用批边界、配置 CAS、预览零写入、
//! 200k 历史的有界批报告与游标续跑。

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use api::ApiState;
use api::maintenance::{MaintenancePolicy, run, seed_maintenance_defs};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use jobs::{JobKind, PayloadUpdate};
use rusqlite::params;
use serde_json::{Value, json};
use tower::ServiceExt;

#[path = "maintenance_support/mod.rs"]
mod support;
use support::{DAY, temp_state, unix_now};

fn data_dir(state: &ApiState) -> std::path::PathBuf {
    support::data_dir(state)
}

fn retention_def(state: &ApiState) -> jobs::JobDef {
    state
        .jobs()
        .lock()
        .list_defs()
        .unwrap()
        .into_iter()
        .find(|def| def.kind == JobKind::DataRetention)
        .expect("默认注册必须包含 data_retention")
}

fn enable_with_payload(state: &ApiState, def: &jobs::JobDef, payload: &Value) {
    let queue = state.jobs();
    let guard = queue.lock();
    assert_eq!(
        guard
            .set_def_payload_cas(def.id, &def.payload, &payload.to_string())
            .unwrap(),
        PayloadUpdate::Updated
    );
    guard.set_def_enabled_by_id(def.id, true).unwrap();
}

fn insert_old_metrics(state: &ApiState, count: usize, recorded_at: i64) {
    let conn = rusqlite::Connection::open(data_dir(state).join("subscribe.db")).unwrap();
    conn.execute_batch("BEGIN").unwrap();
    {
        let mut stmt = conn
            .prepare(
                "INSERT INTO playback_metrics
                    (id, user_id, media_id, tier, engine, ttff_ms, rebuffer_ms,
                     rebuffer_count, seek_count, dropped_frames, total_frames,
                     watched_ms, recorded_at)
                 VALUES (?1, 'user-1', 'media-1', 0, NULL, NULL, 0, 0, 0, NULL, NULL, 0, ?2)",
            )
            .unwrap();
        for index in 0..count {
            stmt.execute(params![format!("old-{index:06}"), recorded_at])
                .unwrap();
        }
    }
    conn.execute_batch("COMMIT").unwrap();
}

fn metric_count(state: &ApiState) -> i64 {
    rusqlite::Connection::open(data_dir(state).join("subscribe.db"))
        .unwrap()
        .query_row("SELECT COUNT(*) FROM playback_metrics", [], |row| row.get(0))
        .unwrap()
}

fn table_count(state: &ApiState, db: &str, table: &str) -> i64 {
    rusqlite::Connection::open(data_dir(state).join(db))
        .unwrap()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
        .unwrap()
}

fn cursor_settings(state: &ApiState) -> i64 {
    rusqlite::Connection::open(data_dir(state).join("app.db"))
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM settings WHERE key LIKE 'maintenance.data_retention.cursor.%'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn claim_for(state: &ApiState, def_id: domain::JobDefId, now: i64) -> jobs::Job {
    let queue = state.jobs();
    let guard = queue.lock();
    // 首次调用会新建 child；后续调用会复用上一次成功时生成的 follow-up（run_after
    // 被 ensure_scheduled_for(0) 提前到 0）。两条路径都通过 claim 取 running job。
    let created = guard.ensure_scheduled_for(0, def_id).unwrap();
    let claimed = guard.claim(now, 1).unwrap();
    assert_eq!(claimed.len(), 1, "必须正好领取一个维护任务");
    assert_eq!(claimed[0].kind, JobKind::DataRetention);
    if let Some(expected) = created.first() {
        assert_eq!(claimed[0].id, expected.id);
    }
    claimed.into_iter().next().unwrap()
}

fn real_payload() -> Value {
    json!({
        "version": 1,
        "dry_run": false,
        "qoe_days": 90,
        "transient_cache_days": 90,
        "probe_days": 90,
    })
}

#[test]
fn new_data_retention_is_disabled_and_dry_run() {
    let (_tmp, state) = temp_state(Arc::new(api::maintenance::SqliteExecutor));
    let def = retention_def(&state);

    assert!(!def.enabled, "新类别默认关闭，生产启用需用户显式确认");
    assert_eq!(def.kind.as_str(), "data_retention");
    assert!(def.kind.is_maintenance());
    assert_eq!(def.schedule, Some(jobs::Schedule::Interval { secs: 86_400 }));

    match MaintenancePolicy::parse(def.kind, &def.payload).unwrap() {
        MaintenancePolicy::DataRetention(policy) => {
            assert_eq!(policy.version, 1);
            assert!(policy.dry_run);
            assert_eq!(policy.qoe_days, 90);
            assert_eq!(policy.transient_cache_days, 90);
            assert_eq!(policy.probe_days, 90);
        }
        other => panic!("data_retention payload 解析成了错误策略: {other:?}"),
    }
}

#[test]
fn invalid_version_or_range_is_rejected() {
    let kind = JobKind::DataRetention;
    let valid = json!({
        "version": 1, "dry_run": true,
        "qoe_days": 90, "transient_cache_days": 90, "probe_days": 90,
    });
    assert!(MaintenancePolicy::parse(kind, &valid.to_string()).is_ok());

    for invalid in [
        json!({"version": 2, "dry_run": true, "qoe_days": 90, "transient_cache_days": 90, "probe_days": 90}),
        json!({"version": 1, "dry_run": true, "qoe_days": 0, "transient_cache_days": 90, "probe_days": 90}),
        json!({"version": 1, "dry_run": true, "qoe_days": 36_501, "transient_cache_days": 90, "probe_days": 90}),
        json!({"version": 1, "dry_run": true, "qoe_days": 90, "probe_days": 90}),
        json!({"version": 1, "dry_run": true, "qoe_days": 90, "transient_cache_days": 90, "probe_days": 90, "extra": 1}),
    ] {
        assert!(
            MaintenancePolicy::parse(kind, &invalid.to_string()).is_err(),
            "必须拒绝: {invalid}"
        );
    }
}

#[test]
fn restart_does_not_overwrite_user_config() {
    let (_tmp, state) = temp_state(Arc::new(api::maintenance::SqliteExecutor));
    let def = retention_def(&state);
    let custom = json!({
        "version": 1,
        "dry_run": false,
        "qoe_days": 30,
        "transient_cache_days": 7,
        "probe_days": 365,
    });
    enable_with_payload(&state, &def, &custom);

    {
        let queue = state.jobs();
        let guard = queue.lock();
        seed_maintenance_defs(&guard).unwrap();
    }

    let after = state.jobs().lock().def(def.id).unwrap().unwrap();
    assert!(after.enabled, "重启不得覆盖用户启用状态");
    assert_eq!(
        serde_json::from_str::<Value>(&after.payload).unwrap(),
        custom,
        "重启不得覆盖用户策略"
    );
    assert_eq!(after.schedule, Some(jobs::Schedule::Interval { secs: 86_400 }));
}

#[test]
fn disable_after_first_batch_stops_second() {
    let (_tmp, state) = temp_state(Arc::new(api::maintenance::SqliteExecutor));
    let now = unix_now();
    insert_old_metrics(&state, 10_000, now - 100 * DAY);
    let def = retention_def(&state);
    enable_with_payload(&state, &def, &real_payload());

    let job = claim_for(&state, def.id, now);
    let state_clone = state.clone();
    let monitor = thread::spawn(move || {
        let conn =
            rusqlite::Connection::open(data_dir(&state_clone).join("subscribe.db")).unwrap();
        for _ in 0..5_000 {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM playback_metrics WHERE id LIKE 'old-%'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            if count < 10_000 {
                state_clone
                    .jobs()
                    .lock()
                    .set_def_enabled_by_id(def.id, false)
                    .unwrap();
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("monitor 未观察到第一批删除");
    });

    run(&state, &job).unwrap();
    monitor.join().unwrap();

    let report = support::progress_of(&state, &job);
    let batches = report["batches"].as_u64().unwrap();
    assert!(batches >= 1, "至少完成第一批: {report}");
    assert!(batches < 20, "禁用后必须提前停止: {report}");
    assert_eq!(
        report["deleted"].as_u64().unwrap(),
        batches * 500,
        "已提交批次的删除数量必须一致: {report}"
    );
    assert!(
        report["note"].as_str().unwrap_or("").contains("禁用"),
        "报告需说明因任务定义禁用而停止: {report}"
    );
    assert!(metric_count(&state) > 0, "必须留下未删除的超龄行");
}

#[test]
fn two_hundred_thousand_qoe_history_reports_bounded_batches_and_resumes() {
    let (_tmp, state) = temp_state(Arc::new(api::maintenance::SqliteExecutor));
    let now = unix_now();
    insert_old_metrics(&state, 200_000, now - 100 * DAY);
    let def = retention_def(&state);
    enable_with_payload(&state, &def, &real_payload());

    let first = claim_for(&state, def.id, now);
    run(&state, &first).unwrap();
    let report = support::progress_of(&state, &first);

    assert_eq!(report["batches"], 20, "单轮批数上限 20: {report}");
    assert_eq!(report["examined"], 10_000, "20 × 500 窗口: {report}");
    assert_eq!(report["deleted"], 10_000, "20 × 500 删除: {report}");
    assert_eq!(report["incomplete"], true, "200k 历史不可能一轮完成: {report}");
    assert_eq!(report["remaining"], true, "预算不足必须显式 remaining: {report}");
    assert_eq!(report["budget_exhausted"], true);
    let qoe = &report["classes"][0];
    assert_eq!(qoe["class"], "qoe");
    assert_eq!(qoe["incomplete"], true);
    assert!(qoe["resume_after"].is_object(), "必须返回续跑游标: {qoe}");
    assert_eq!(metric_count(&state), 190_000);

    state.jobs().lock().succeed_claimed(&first, now).unwrap();
    let second = claim_for(&state, def.id, now);
    run(&state, &second).unwrap();
    let report = support::progress_of(&state, &second);
    assert_eq!(report["deleted"], 10_000, "持久游标必须继续下一窗口: {report}");
    assert_eq!(metric_count(&state), 180_000);
}

#[tokio::test]
async fn preview_is_read_only_and_reports_classes() {
    let (_tmp, state) = temp_state(Arc::new(api::maintenance::SqliteExecutor));
    let now = unix_now();
    insert_old_metrics(&state, 3, now - 100 * DAY);
    {
        let store = state.store();
        let store = store.lock();
        store
            .put_catalog_cache("tmdb", "/search/movie?query=avatar", "{}", now - 100 * DAY, None)
            .unwrap();
    }
    {
        let conn = rusqlite::Connection::open(data_dir(&state).join("library.db")).unwrap();
        for (id, ts) in [("p-10", 10), ("p-20", 20), ("p-30", 30), ("p-40", 40)] {
            conn.execute(
                "INSERT INTO probe_jobs
                    (id, kind, media_id, season, scope_key, status, total, completed,
                     succeeded, failed, partial, error, created_at_ms, started_at_ms, finished_at_ms)
                 VALUES (?1, 'metadata', 'm', NULL, 'scope-p', 'succeeded', 1, 1, 1, 0, 0, NULL, ?2, ?2, ?2)",
                params![id, ts],
            )
            .unwrap();
        }
    }
    let def = retention_def(&state);
    let app = api::router(state.clone());
    let token = support::TOKEN;

    // 预览请求故意带 dry_run=false：预览必须自己强制只读，不能信客户端开关。
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/jobs/{}/maintenance-preview", def.id))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(
                    json!({
                        "payload": {
                            "version": 1, "dry_run": false,
                            "qoe_days": 90, "transient_cache_days": 90, "probe_days": 90,
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let envelope: Value = serde_json::from_slice(&bytes).unwrap();
    let data = &envelope["data"];
    assert_eq!(data["examined"], 8, "3 QoE + 1 catalog + 4 probe: {data}");
    assert_eq!(data["truncated"], false);
    assert_eq!(data["classes"].as_array().unwrap().len(), 3);

    assert_eq!(metric_count(&state), 3, "预览不得删除 QoE 行");
    assert_eq!(table_count(&state, "catalog.db", "catalog_cache"), 1);
    assert_eq!(table_count(&state, "library.db", "probe_jobs"), 4);
    assert_eq!(cursor_settings(&state), 0, "预览不得写游标");
}

#[tokio::test]
async fn maintenance_config_accepts_data_retention_and_enforces_cas() {
    let (_tmp, state) = temp_state(Arc::new(api::maintenance::SqliteExecutor));
    let def = retention_def(&state);
    let expected: Value = serde_json::from_str(&def.payload).unwrap();
    let app = api::router(state.clone());
    let token = support::TOKEN;
    let valid = json!({
        "version": 1,
        "dry_run": false,
        "qoe_days": 30,
        "transient_cache_days": 14,
        "probe_days": 120,
    });

    let put = |body: Value| {
        let app = app.clone();
        let uri = format!("/api/v1/jobs/{}/maintenance-config", def.id);
        async move {
            let response = app
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri(uri)
                        .header("content-type", "application/json")
                        .header("authorization", format!("Bearer {token}"))
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            (status, serde_json::from_slice::<Value>(&bytes).unwrap())
        }
    };

    let (status, response) = put(json!({
        "payload": valid.clone(),
        "expected_payload": expected.clone(),
        "interval_secs": 86_400,
    }))
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["data"]["payload"]["qoe_days"], 30);

    // 旧令牌再次提交 → 409，不允许覆盖。
    let (status, _) = put(json!({
        "payload": valid.clone(),
        "expected_payload": expected.clone(),
        "interval_secs": 86_400,
    }))
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // 越界窗口 → 400，且不落库。
    let (status, _) = put(json!({
        "payload": {
            "version": 1, "dry_run": false,
            "qoe_days": 0, "transient_cache_days": 14, "probe_days": 120,
        },
        "expected_payload": valid.clone(),
        "interval_secs": 86_400,
    }))
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
