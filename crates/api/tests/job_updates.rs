//! C4：任务计数共享 watcher 生命周期 + jobs 列表在任务运行期间可响应。

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use api::job_updates::JobUpdates;
use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::Request;
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use jobs::{JobKind, NewDef};
use serde_json::Value;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

fn make_state(root: &Path) -> ApiState {
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

async fn wait_for<F: FnMut() -> bool>(mut check: F, timeout: Duration, what: &str) {
    let deadline = Instant::now() + timeout;
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn get_json(app: &axum::Router, uri: &str) -> Value {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), 200, "{uri}");
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    body.get("data").cloned().unwrap_or(Value::Null)
}

fn add_finished_child(state: &ApiState, name: &str) -> jobs::JobDef {
    let queue = state.jobs();
    let def = queue
        .lock()
        .upsert_def(NewDef {
            kind: JobKind::CatalogRefresh,
            name: name.into(),
            enabled: true,
            schedule: None,
            payload: "{}".into(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap();
    let child = queue
        .lock()
        .ensure_scheduled_for(0, def.id)
        .unwrap()
        .remove(0);
    // 队列里可能还有实例种子任务：claim 足够多，确认本 def 的子任务已进入 running。
    let claimed = queue.lock().claim(1, 50).unwrap();
    assert!(
        claimed.iter().any(|job| job.id == child.id),
        "child must be claimed"
    );
    queue.lock().succeed(child.id, 1).unwrap();
    def
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_subscribers_share_one_poller_and_propagate_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let updates = Arc::new(JobUpdates::new());
    let state = make_state(tmp.path()).with_job_updates(updates.clone());
    let queue = state.jobs();

    let first = updates.subscribe(queue.clone()).await.unwrap();
    let second = updates.subscribe(queue.clone()).await.unwrap();
    assert_eq!(updates.subscriber_count(), 2);
    let initial = first.borrow().count;
    assert_eq!(second.borrow().count, initial, "订阅即拿到真实当前计数");
    assert_eq!(initial, queue.lock().count_finished_since(0).unwrap());

    // 两个订阅者只应共用一个 2 秒 poller：4.6s 内轮询 1..=3 次。
    tokio::time::sleep(Duration::from_millis(4600)).await;
    let shared_polls = updates.poll_count();
    assert!(
        (1..=3).contains(&shared_polls),
        "two subscribers must share one poller, polls={shared_polls}"
    );

    // 计数变化传播到两个订阅者。
    let _def = add_finished_child(&state, "shared-watch");
    let expected = initial + 1;
    wait_for(
        || first.borrow().count == expected,
        Duration::from_secs(6),
        "first subscriber count",
    )
    .await;
    wait_for(
        || second.borrow().count == expected,
        Duration::from_secs(6),
        "second subscriber count",
    )
    .await;

    // 全部 receiver 掉落后 poller 停止。
    drop(first);
    drop(second);
    wait_for(
        || updates.subscriber_count() == 0,
        Duration::from_secs(1),
        "receivers dropped",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(2300)).await;
    let stopped_at = updates.poll_count();
    tokio::time::sleep(Duration::from_millis(3000)).await;
    assert_eq!(
        updates.poll_count(),
        stopped_at,
        "poller must stop once every receiver is gone"
    );

    // 再次订阅可重启。
    let _restarted = updates.subscribe(queue.clone()).await.unwrap();
    wait_for(
        || updates.poll_count() > stopped_at,
        Duration::from_secs(4),
        "poller restart",
    )
    .await;
}

#[tokio::test]
async fn job_stream_reports_failure_when_first_count_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state);
    // 破坏计数来源：SSE 计数读的是 job_change_state 单行，不是重 COUNT(jobs)。
    // 计数查询失败时，首个事件必须报错，而不是伪造 count=0。
    let connection = rusqlite::Connection::open(tmp.path().join("data/jobs.db")).unwrap();
    connection
        .execute_batch("DROP TABLE job_change_state;")
        .unwrap();

    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/jobs/stream")
        .header("authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(
        response.status(),
        500,
        "首次计数失败必须报错，而不是伪造 count=0"
    );
}

#[tokio::test]
async fn jobs_list_reports_live_child_while_job_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let queue = state.jobs();
    let def = queue
        .lock()
        .upsert_def(NewDef {
            kind: JobKind::CatalogRefresh,
            name: "running-def".into(),
            enabled: true,
            schedule: None,
            payload: "{}".into(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap();
    let child = queue
        .lock()
        .ensure_scheduled_for(0, def.id)
        .unwrap()
        .remove(0);
    let claimed = queue.lock().claim(1, 50).unwrap();
    assert!(
        claimed.iter().any(|job| job.id == child.id),
        "child must be claimed"
    );

    let listed = get_json(&app, "/api/v1/jobs").await;
    let item = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == def.id.to_string())
        .unwrap_or_else(|| panic!("def missing from list: {listed}"))
        .clone();
    assert_eq!(item["last_status"], "running");
    assert_eq!(item["name"], "running-def");

    // active_only 仍只保留有 live child 的 def。
    let active = get_json(&app, "/api/v1/jobs?active_only=true").await;
    assert!(
        active
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == def.id.to_string()),
        "{active}"
    );
}
