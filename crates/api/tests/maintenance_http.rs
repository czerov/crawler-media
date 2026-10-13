//! DB8 Task 5：维护配置 HTTP 合同（保存 CAS、只读预览、报告读取、手动执行闸门）。
//!
//! 断言全部走公开 seam：`/api/v1` Router + `jobs::Queue` + `Store` 的公开读方法；
//! 行数与 payload 的"没变"用只读 SQLite 连接直接比对（setup/观测用 raw SQL 是仓库既有约定）。

use std::path::Path;
use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{User, UserId, UserRole};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use parking_lot::Mutex;
use rusqlite::Connection;
use serde_json::{Value, json};
use tower::ServiceExt;

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

fn test_state(root: &Path) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
}

/// 维护 def 的 payload 默认值（与 `api::maintenance::default_payload` 一致），
/// 用作 CAS 的 `expected_payload`。
fn db_payload() -> Value {
    json!({ "version": 1, "vacuum_enabled": false })
}

fn retention_payload(days: u32, dry_run: bool) -> Value {
    json!({ "version": 1, "retention_days": days, "dry_run": dry_run })
}

async fn call(app: &axum::Router, method: &str, uri: &str, token: &str, body: Value) -> Value {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if !token.is_empty() {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let envelope: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status, StatusCode::OK, "{method} {uri} -> {envelope}");
    assert_eq!(envelope["ok"], true, "{method} {uri} -> {envelope}");
    envelope["data"].clone()
}

async fn call_err(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if !token.is_empty() {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let envelope: Value = serde_json::from_slice(&bytes).unwrap();
    (status, envelope)
}

fn list_jobs(app: &axum::Router) -> impl std::future::Future<Output = Value> + '_ {
    call(app, "GET", "/api/v1/jobs", "management-secret", Value::Null)
}

fn find_def(app_state: &ApiState, kind: jobs::JobKind) -> jobs::JobDef {
    app_state
        .jobs()
        .lock()
        .list_defs()
        .unwrap()
        .into_iter()
        .find(|def| def.kind == kind)
        .unwrap_or_else(|| panic!("缺少 {kind:?} 维护 def"))
}

/// 在目标 def 下插入一条超龄终态 job（raw SQL setup，断言仍走公开 seam）。
fn seed_terminal_job(root: &Path, def_id: domain::JobDefId, finished_at: i64) -> String {
    let conn = Connection::open(root.join("data/jobs.db")).unwrap();
    let id = domain::JobId::new().to_string();
    conn.execute(
        "INSERT INTO jobs (id, def_id, kind, payload, status, attempt, run_after, finished_at)
         VALUES (?1, ?2, 'jobs_retention', '{}', 'succeeded', 0, ?3, ?3)",
        rusqlite::params![id, def_id.to_string(), finished_at],
    )
    .unwrap();
    id
}

/// 给目标 def 插入一条带报告的超龄终态 job。
fn seed_terminal_job_with_report(
    root: &Path,
    def_id: domain::JobDefId,
    finished_at: i64,
    progress: &str,
) -> String {
    let conn = Connection::open(root.join("data/jobs.db")).unwrap();
    let id = domain::JobId::new().to_string();
    conn.execute(
        "INSERT INTO jobs (id, def_id, kind, payload, status, attempt, run_after, finished_at, progress)
         VALUES (?1, ?2, 'jobs_retention', '{}', 'succeeded', 0, ?3, ?3, ?4)",
        rusqlite::params![id, def_id.to_string(), finished_at, progress],
    )
    .unwrap();
    id
}

/// `def_id IS NULL` 的历史（清理时不受 last-child 保护，便于构造"计数下降"）。
fn seed_orphan_terminal(root: &Path, finished_at: i64) -> String {
    let conn = Connection::open(root.join("data/jobs.db")).unwrap();
    let id = domain::JobId::new().to_string();
    conn.execute(
        "INSERT INTO jobs (id, def_id, kind, payload, status, attempt, run_after, finished_at)
         VALUES (?1, NULL, 'jobs_retention', '{}', 'succeeded', 0, ?2, ?2)",
        rusqlite::params![id, finished_at],
    )
    .unwrap();
    id
}

fn row_counts(root: &Path) -> (i64, i64) {
    let conn = Connection::open(root.join("data/jobs.db")).unwrap();
    (
        conn.query_row("SELECT COUNT(*) FROM job_defs", [], |row| row.get(0))
            .unwrap(),
        conn.query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get(0))
            .unwrap(),
    )
}

fn stored_payload(root: &Path, def_id: domain::JobDefId) -> String {
    Connection::open(root.join("data/jobs.db"))
        .unwrap()
        .query_row(
            "SELECT payload FROM job_defs WHERE id = ?1",
            rusqlite::params![def_id.to_string()],
            |row| row.get(0),
        )
        .unwrap()
}

fn seed_member(state: &ApiState) -> String {
    let store = state.store();
    let id = UserId::new();
    store
        .lock()
        .insert_user_with_password(
            &User {
                id,
                login: "member".into(),
                enabled: true,
                role: UserRole::Member,
            },
            "member-pass",
        )
        .unwrap();
    let token = "member-session".to_string();
    store.lock().set_user_token(id, &token).unwrap();
    token
}

#[tokio::test]
async fn member_requests_are_rejected_by_admin_layer() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let app = router(state.clone());
    let token = seed_member(&state);
    let id = find_def(&state, jobs::JobKind::JobsRetention).id;

    let (save_status, save) = call_err(
        &app,
        "PUT",
        &format!("/api/v1/jobs/{id}/maintenance-config"),
        &token,
        json!({ "payload": retention_payload(30, true) }),
    )
    .await;
    assert_eq!(save_status, StatusCode::FORBIDDEN, "{save}");

    let (preview_status, preview) = call_err(
        &app,
        "POST",
        &format!("/api/v1/jobs/{id}/maintenance-preview"),
        &token,
        json!({ "payload": retention_payload(30, true) }),
    )
    .await;
    assert_eq!(preview_status, StatusCode::FORBIDDEN, "{preview}");
}

#[tokio::test]
async fn admin_saves_config_and_reads_back_the_new_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let app = router(state.clone());
    let id = find_def(&state, jobs::JobKind::DatabaseMaintenance).id;

    let saved = call(
        &app,
        "PUT",
        &format!("/api/v1/jobs/{id}/maintenance-config"),
        "management-secret",
        json!({ "payload": db_payload(), "expected_payload": db_payload() }),
    )
    .await;
    assert_eq!(saved["payload"]["vacuum_enabled"], false, "{saved}");
    assert!(saved["payload_token"].is_string(), "{saved}");

    let def = state.jobs().lock().def(id).unwrap().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&def.payload).unwrap(),
        json!({ "version": 1, "vacuum_enabled": false })
    );

    let listed = list_jobs(&app).await;
    let item = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == id.to_string())
        .unwrap()
        .clone();
    assert_eq!(item["maintenance"]["vacuum_enabled"], false, "{item}");
    assert!(item["last_report"].is_null(), "{item}");
}

#[tokio::test]
async fn stale_expected_payload_is_a_conflict_and_does_not_write() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let app = router(state.clone());
    let id = find_def(&state, jobs::JobKind::JobsRetention).id;
    let before = stored_payload(tmp.path(), id);

    let (status, error) = call_err(
        &app,
        "PUT",
        &format!("/api/v1/jobs/{id}/maintenance-config"),
        "management-secret",
        json!({
            "payload": retention_payload(30, true),
            "expected_payload": retention_payload(7, false),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "job.conflict", "{error}");
    assert_eq!(stored_payload(tmp.path(), id), before, "409 不得写库");
}

#[tokio::test]
async fn invalid_or_unrelated_configs_are_rejected_without_persisting() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let app = router(state.clone());
    let id = find_def(&state, jobs::JobKind::JobsRetention).id;
    let before = stored_payload(tmp.path(), id);

    // 越界 retention / 未知版本 / 未知字段 / 坏 JSON 形态（缺 version）。
    let bad = [
        json!({ "version": 1, "retention_days": 0, "dry_run": false }),
        json!({ "version": 1, "retention_days": 36501, "dry_run": false }),
        json!({ "version": 2, "retention_days": 30, "dry_run": false }),
        json!({ "version": 1, "retention_days": 30, "dry_run": false, "surprise": 1 }),
        json!({ "retention_days": 30, "dry_run": false }),
    ];
    for payload in bad {
        let (status, error) = call_err(
            &app,
            "PUT",
            &format!("/api/v1/jobs/{id}/maintenance-config"),
            "management-secret",
            json!({ "payload": payload, "expected_payload": json!({ "version": 1, "retention_days": 90, "dry_run": false }) }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{payload} -> {error}");
        assert_eq!(stored_payload(tmp.path(), id), before, "{payload}");
    }

    // 无关 kind（非维护 def）→ 400。
    let unrelated = state
        .jobs()
        .lock()
        .upsert_def(jobs::NewDef {
            kind: jobs::JobKind::CatalogRefresh,
            name: "catalog".into(),
            enabled: true,
            schedule: None,
            payload: "{}".into(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap();
    let (status, error) = call_err(
        &app,
        "PUT",
        &format!("/api/v1/jobs/{}/maintenance-config", unrelated.id),
        "management-secret",
        json!({ "payload": retention_payload(30, false) }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");

    // 未知 def → 404。
    let missing = domain::JobDefId::new();
    let (status, error) = call_err(
        &app,
        "PUT",
        &format!("/api/v1/jobs/{missing}/maintenance-config"),
        "management-secret",
        json!({ "payload": retention_payload(30, false) }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
    assert_eq!(stored_payload(tmp.path(), id), before, "拒绝路径不得写库");
}

#[tokio::test]
async fn maintenance_interval_is_validated_and_updates_the_schedule() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let app = router(state.clone());
    let id = find_def(&state, jobs::JobKind::DatabaseMaintenance).id;

    for secs in [0u64, 3599, 31_536_001] {
        let (status, error) = call_err(
            &app,
            "PUT",
            &format!("/api/v1/jobs/{id}/maintenance-config"),
            "management-secret",
            json!({
                "payload": db_payload(),
                "expected_payload": db_payload(),
                "interval_secs": secs,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{secs} -> {error}");
    }

    for secs in [3600u64, 86_400] {
        let saved = call(
            &app,
            "PUT",
            &format!("/api/v1/jobs/{id}/maintenance-config"),
            "management-secret",
            json!({
                "payload": db_payload(),
                "expected_payload": db_payload(),
                "interval_secs": secs,
            }),
        )
        .await;
        assert_eq!(saved["schedule"]["interval_secs"], secs, "{saved}");
        assert_eq!(
            state.jobs().lock().def(id).unwrap().unwrap().schedule,
            Some(jobs::Schedule::Interval { secs })
        );
    }
}

#[tokio::test]
async fn preview_is_read_only_and_reports_bounded_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let app = router(state.clone());
    let id = find_def(&state, jobs::JobKind::JobsRetention).id;
    seed_terminal_job(tmp.path(), id, 1_000);
    let before = row_counts(tmp.path());

    let preview = call(
        &app,
        "POST",
        &format!("/api/v1/jobs/{id}/maintenance-preview"),
        "management-secret",
        json!({ "payload": retention_payload(90, false) }),
    )
    .await;
    assert_eq!(preview["kind"], "jobs_retention", "{preview}");
    // 种子 job 是该 def 的最新终态 → 属于 last-child 保护组。修复 C1 后受保护行
    // **不进候选窗口**，因此可删候选为 0，保护数单独由 `protected` 报告。
    assert_eq!(preview["scanned"], 0, "{preview}");
    assert_eq!(preview["candidates"], 0, "{preview}");
    assert_eq!(preview["protected"], 1, "{preview}");
    assert_eq!(preview["truncated"], false, "{preview}");
    assert_eq!(row_counts(tmp.path()), before, "preview 不得写 DB");

    let unrelated = state
        .jobs()
        .lock()
        .upsert_def(jobs::NewDef {
            kind: jobs::JobKind::CatalogRefresh,
            name: "catalog".into(),
            enabled: true,
            schedule: None,
            payload: "{}".into(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap();
    let (status, error) = call_err(
        &app,
        "POST",
        &format!("/api/v1/jobs/{}/maintenance-preview", unrelated.id),
        "management-secret",
        json!({ "payload": retention_payload(30, false) }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
}

#[tokio::test]
async fn preview_marks_truncated_when_the_budget_is_exhausted() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let app = router(state.clone());
    let id = find_def(&state, jobs::JobKind::JobsRetention).id;
    for i in 0..700i64 {
        seed_orphan_terminal(tmp.path(), i + 1);
    }
    let before = row_counts(tmp.path());

    let preview = call(
        &app,
        "POST",
        &format!("/api/v1/jobs/{id}/maintenance-preview"),
        "management-secret",
        json!({ "payload": retention_payload(90, false) }),
    )
    .await;
    assert_eq!(preview["candidates"], 500, "{preview}");
    assert_eq!(preview["truncated"], true, "{preview}");
    assert_eq!(row_counts(tmp.path()), before, "preview 不得写 DB");
}

#[tokio::test]
async fn run_is_refused_for_disabled_defs_and_queued_defs_report_existing() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let app = router(state.clone());
    let id = find_def(&state, jobs::JobKind::JobsRetention).id;

    let toggled = call(
        &app,
        "PATCH",
        &format!("/api/v1/jobs/{id}"),
        "management-secret",
        json!({ "enabled": false }),
    )
    .await;
    assert_eq!(toggled["enabled"], false, "{toggled}");

    let (status, error) = call_err(
        &app,
        "POST",
        &format!("/api/v1/jobs/{id}/run"),
        "management-secret",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "job.disabled", "{error}");

    call(
        &app,
        "PATCH",
        &format!("/api/v1/jobs/{id}"),
        "management-secret",
        json!({ "enabled": true }),
    )
    .await;
    let first = call(
        &app,
        "POST",
        &format!("/api/v1/jobs/{id}/run"),
        "management-secret",
        Value::Null,
    )
    .await;
    assert_eq!(first["queued"], true, "{first}");
    let second = call(
        &app,
        "POST",
        &format!("/api/v1/jobs/{id}/run"),
        "management-secret",
        Value::Null,
    )
    .await;
    assert_eq!(second["queued"], false, "重复触发不得误报新增: {second}");
    assert_eq!(second["status"], "already_queued", "{second}");

    let queued = state
        .jobs()
        .lock()
        .get_live_child(id)
        .unwrap()
        .expect("立即执行后应有排队中的维护任务");
    assert_eq!(queued.status, jobs::JobStatus::Queued);
    assert_eq!(queued.run_after, 0);
    let conn = rusqlite::Connection::open(tmp.path().join("data/jobs.db")).unwrap();
    let changed = conn
        .execute(
            "UPDATE jobs SET status = 'running', started_at = ?2 WHERE id = ?1 AND status = 'queued'",
            rusqlite::params![queued.id.to_string(), 1_000_i64],
        )
        .unwrap();
    assert_eq!(changed, 1, "测试必须把刚排队的维护任务标成运行中");
    let while_running = call(
        &app,
        "POST",
        &format!("/api/v1/jobs/{id}/run"),
        "management-secret",
        Value::Null,
    )
    .await;
    assert_eq!(while_running["status"], "already_running", "{while_running}");
    assert_eq!(while_running["queued"], false, "{while_running}");
    state.jobs().lock().succeed(queued.id, 1_100).unwrap();
    let follow_up = state.jobs().lock().get_live_child(id).unwrap();
    assert!(
        follow_up.as_ref().is_none_or(|job| job.run_after > 1_100),
        "运行中再点立即执行不得留下 run_after=0 的新任务: {follow_up:?}"
    );
}

#[tokio::test]
async fn jobs_list_exposes_the_latest_maintenance_report() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let app = router(state.clone());
    let id = find_def(&state, jobs::JobKind::JobsRetention).id;
    let report = r#"{"kind":"jobs_retention","policy_version":1,"deleted":3,"protected":1,"budget_exhausted":false}"#;
    seed_terminal_job_with_report(tmp.path(), id, 1_000, report);

    let listed = list_jobs(&app).await;
    let item = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == id.to_string())
        .unwrap()
        .clone();
    assert_eq!(item["last_report"], report, "{item}");
    assert_eq!(item["maintenance"]["retention_days"], 90, "{item}");
}

#[tokio::test]
async fn missing_concurrency_token_is_rejected_without_persisting() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let app = router(state.clone());
    let id = find_def(&state, jobs::JobKind::JobsRetention).id;
    let before = stored_payload(tmp.path(), id);

    let (status, error) = call_err(
        &app,
        "PUT",
        &format!("/api/v1/jobs/{id}/maintenance-config"),
        "management-secret",
        json!({ "payload": retention_payload(30, false) }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["error"]["code"], "job.missing_token", "{error}");
    assert_eq!(stored_payload(tmp.path(), id), before, "缺令牌不得写库");
}

#[tokio::test]
async fn jobs_list_exposes_maintenance_policy_report_and_keeps_other_defs_compatible() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let app = router(state.clone());
    let retention_id = find_def(&state, jobs::JobKind::JobsRetention).id;
    seed_terminal_job(tmp.path(), retention_id, 1_000);

    let listed = list_jobs(&app).await;
    let maintenance = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == retention_id.to_string())
        .unwrap()
        .clone();
    assert_eq!(
        maintenance["maintenance"]["retention_days"], 90,
        "{maintenance}"
    );
    assert_eq!(
        maintenance["maintenance"]["dry_run"], false,
        "{maintenance}"
    );
    assert!(maintenance["last_report"].is_null(), "{maintenance}");

    let unrelated = state
        .jobs()
        .lock()
        .upsert_def(jobs::NewDef {
            kind: jobs::JobKind::CatalogRefresh,
            name: "catalog".into(),
            enabled: true,
            schedule: None,
            payload: "{}".into(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap();
    let listed = list_jobs(&app).await;
    let plain = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == unrelated.id.to_string())
        .unwrap()
        .clone();
    assert!(plain.get("maintenance").is_none(), "{plain}");
    assert!(plain.get("last_report").is_none(), "{plain}");
    assert!(plain.get("payload_token").is_none(), "{plain}");
    // 既有字段不变（表征断言）。
    assert_eq!(plain["kind"], "catalog_refresh");
    assert_eq!(plain["name"], "catalog");
    assert_eq!(plain["enabled"], true);
    assert_eq!(plain["payload"], json!({}));
    assert_eq!(plain["schedule"], Value::Null);
    assert_eq!(plain["concurrency_key"], Value::Null);
    assert_eq!(plain["last_status"], Value::Null);
    assert_eq!(plain["last_finished_at"], Value::Null);
    assert_eq!(plain["last_error"], Value::Null);
    assert_eq!(plain["next_run_after"], Value::Null);
}

#[tokio::test]
async fn config_change_only_affects_the_next_run() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let app = router(state.clone());
    let id = find_def(&state, jobs::JobKind::JobsRetention).id;

    let queued = call(
        &app,
        "POST",
        &format!("/api/v1/jobs/{id}/run"),
        "management-secret",
        Value::Null,
    )
    .await;
    assert_eq!(queued["queued"], true, "{queued}");
    let running = state.jobs().lock().claim(0, 50).unwrap();
    let claimed = running
        .into_iter()
        .find(|job| job.def_id == Some(id))
        .expect("维护 job 应已入队");

    call(
        &app,
        "PUT",
        &format!("/api/v1/jobs/{id}/maintenance-config"),
        "management-secret",
        json!({
            "payload": retention_payload(7, true),
            "expected_payload": retention_payload(90, false),
        }),
    )
    .await;

    let still_old = state.jobs().lock().get(claimed.id).unwrap().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&still_old.payload).unwrap(),
        retention_payload(90, false),
        "运行中的 job 必须保留自己那份策略快照"
    );
    let def = state.jobs().lock().def(id).unwrap().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&def.payload).unwrap(),
        retention_payload(7, true)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watcher_still_emits_when_terminal_count_drops() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let updates = Arc::new(api::job_updates::JobUpdates::new());
    let state = state.with_job_updates(updates.clone());
    let queue = state.jobs();
    for i in 0..5i64 {
        seed_orphan_terminal(tmp.path(), 1_000 + i);
    }

    let mut receiver = updates.subscribe(queue.clone()).await.unwrap();
    let before = receiver.borrow().count;
    assert_eq!(before, 5, "订阅即拿到真实当前计数");

    // 清理（计数下降）：删除全部超龄终态、无 def 的历史。
    let batch = queue
        .lock()
        .prune_terminal_jobs(100_000, 500, false)
        .unwrap();
    assert!(batch.deleted > 0, "{batch:?}");
    assert_eq!(queue.lock().count_finished_since(0).unwrap(), 0);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    let mut observed = None;
    while std::time::Instant::now() < deadline {
        if receiver.borrow().count == 0 {
            observed = Some(0);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        observed,
        Some(0),
        "terminal 计数下降也必须触发更新（不得假设计数单调递增）"
    );
    assert!(updates.poll_count() > 0, "共享 poller 必须实际轮询过");
}

/// DB8 复核 I2：维护任务的周期更新必须使用 maintenance-config 端点，
/// 旧 schedule 端点对维护任务一律返回 409，防并发绕过 CAS。
#[tokio::test]
async fn legacy_schedule_endpoint_enforces_the_maintenance_interval_range() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let app = router(state.clone());
    let id = find_def(&state, jobs::JobKind::DatabaseMaintenance).id;

    for secs in [30u64, 3599, 7_200, 31_536_001] {
        let (status, error) = call_err(
            &app,
            "PUT",
            &format!("/api/v1/jobs/{id}/schedule"),
            "management-secret",
            json!({ "interval_secs": secs }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "维护任务调用旧 schedule 端点必须返回 409: {error}"
        );
        assert_eq!(error["error"]["code"], "job.use_maintenance_config");
    }

    // 非维护任务保持既有契约：30 秒仍然合法。
    let other = find_def(&state, jobs::JobKind::Transfer).id;
    let saved = call(
        &app,
        "PUT",
        &format!("/api/v1/jobs/{other}/schedule"),
        "management-secret",
        json!({ "interval_secs": 30 }),
    )
    .await;
    assert_eq!(
        saved["schedule"]["interval_secs"], 30,
        "非维护任务的周期契约不得改变: {saved}"
    );
}
