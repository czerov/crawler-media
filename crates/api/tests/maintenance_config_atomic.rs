//! T2 测试：全维护配置 CAS + 刷新 queued 快照与统一写入口 (F02/F04)。

use std::path::Path;
use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use domain::JobDefId;
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use jobs::JobKind;
use serde_json::{json, Value};
use tower::ServiceExt;

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("测试不访问网络".into()))
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

async fn call(app: &axum::Router, method: &str, uri: &str, token: &str, body: Value) -> Value {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if !token.is_empty() {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let body_data = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let response = app
        .clone()
        .oneshot(builder.body(body_data).unwrap())
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
    let body_data = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let response = app
        .clone()
        .oneshot(builder.body(body_data).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let envelope: Value = serde_json::from_slice(&bytes).unwrap();
    (status, envelope)
}

#[tokio::test]
async fn queued_job_payload_is_refreshed_when_new_safer_policy_saved() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let app = router(state.clone());
    let now = store::unix_now();

    // 默认维护 def (例如 jobs_retention 策略为 retention_days = 90, dry_run = false)
    let def = state.jobs().lock().list_defs().unwrap()
        .into_iter().find(|d| d.kind == JobKind::JobsRetention).unwrap();

    // 触发排队产生一个 queued child
    state.jobs().lock().ensure_scheduled_for(now + 1000, def.id).unwrap();

    let queued_before = state.jobs().lock().get_live_child(def.id).unwrap().unwrap();
    assert_eq!(queued_before.status, jobs::JobStatus::Queued);
    assert!(queued_before.payload.contains(r#""retention_days":90"#));

    // 管理员将配置改为更安全的策略 (retention_days = 365, dry_run = true)
    let new_policy = json!({
        "version": 1,
        "retention_days": 365,
        "dry_run": true
    });

    let saved = call(
        &app,
        "PUT",
        &format!("/api/v1/jobs/{}/maintenance-config", def.id),
        "management-secret",
        json!({
            "payload": new_policy,
            "expected_payload": serde_json::from_str::<Value>(&def.payload).unwrap(),
            "interval_secs": 86400
        }),
    ).await;
    assert_eq!(saved["kind"], "jobs_retention");

    // 校验：已经排队的 queued child 的 payload 必须被刷新为新策略！
    let queued_after = state.jobs().lock().get_live_child(def.id).unwrap().unwrap();
    assert_eq!(queued_after.status, jobs::JobStatus::Queued);
    assert!(
        queued_after.payload.contains(r#""retention_days":365"#),
        "queued 子任务的 payload 必须更新为新策略，而不是残留旧策略: {}",
        queued_after.payload
    );
    assert!(
        queued_after.payload.contains(r#""dry_run":true"#),
        "queued 子任务的 dry_run 必须更新为 true: {}",
        queued_after.payload
    );
}

#[tokio::test]
async fn legacy_schedule_endpoint_is_rejected_for_maintenance_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let app = router(state.clone());
    let def = state.jobs().lock().list_defs().unwrap()
        .into_iter().find(|d| d.kind == JobKind::JobsRetention).unwrap();

    // 旧的 PUT /api/v1/jobs/{id}/schedule 接口对维护 def 应当被拒绝，要求使用 maintenance-config
    let (status, resp) = call_err(
        &app,
        "PUT",
        &format!("/api/v1/jobs/{}/schedule", def.id),
        "management-secret",
        json!({
            "interval_secs": 7200
        }),
    ).await;
    assert_eq!(status, StatusCode::CONFLICT, "维护任务必须拒绝旧 schedule 端点以防并发绕过 CAS: {resp}");
    assert_eq!(resp["error"]["code"], "job.use_maintenance_config");
}
