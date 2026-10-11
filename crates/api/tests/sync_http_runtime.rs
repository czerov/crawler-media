//! 同步 HTTP / 下载器入口的 async-worker 隔离回归。
//!
//! 复现：notify 诊断发送在 async worker 上直接跑裸 ureq；下载器的
//! `client_for_id` 对非默认实例同步建连，且当前在 spawn 之前 resolve。
//! 两条路径都必须走 `ApiState::http_work` 的 owned blocking 闭包。

use std::path::Path;
use std::sync::{Arc, Condvar, Mutex as StdMutex};
use std::thread::ThreadId;
use std::time::Duration;

use api::blocking_budget::BlockingBudget;
use api::{ApiState, NotifyTransport, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::Torrent;
use downloader::{Downloader, DownloaderError};
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "management-secret";
const LIMITS: &str = "/api/v1/downloaders/limits";
const NOTIFY_TEST: &str = "/api/v1/notify/test";

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

/// 让被注入的 blocking 实现停在闭包内，直到测试显式 release。
struct Gate {
    entered: StdMutex<bool>,
    entered_cv: Condvar,
    open: StdMutex<bool>,
    open_cv: Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            entered: StdMutex::new(false),
            entered_cv: Condvar::new(),
            open: StdMutex::new(false),
            open_cv: Condvar::new(),
        }
    }

    fn enter_and_wait(&self) {
        *self.entered.lock().unwrap() = true;
        self.entered_cv.notify_all();
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.open_cv.wait(open).unwrap();
        }
    }

    fn wait_entered(&self) {
        let mut entered = self.entered.lock().unwrap();
        while !*entered {
            entered = self.entered_cv.wait(entered).unwrap();
        }
    }

    fn entered(&self) -> bool {
        *self.entered.lock().unwrap()
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.open_cv.notify_all();
    }
}

/// 断言失败时也要放行阻塞闭包，避免 runtime 关闭时卡住。
struct ReleaseOnDrop(Arc<Gate>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// telemetry 阻塞并记录真实调用线程。
struct GatedDownloader {
    gate: Arc<Gate>,
    call_thread: StdMutex<Option<ThreadId>>,
}

impl GatedDownloader {
    fn new(gate: Arc<Gate>) -> Self {
        Self {
            gate,
            call_thread: StdMutex::new(None),
        }
    }

    fn call_thread(&self) -> Option<ThreadId> {
        *self.call_thread.lock().unwrap()
    }

    fn note_call(&self) {
        *self.call_thread.lock().unwrap() = Some(std::thread::current().id());
        self.gate.enter_and_wait();
    }
}

impl Downloader for GatedDownloader {
    fn add(&self, _torrent: &Torrent) -> Result<(), DownloaderError> {
        Ok(())
    }

    fn completed_files(
        &self,
        _torrent: &Torrent,
    ) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        Ok(Vec::new())
    }

    fn get_speed_limits(&self) -> Result<(u64, u64), DownloaderError> {
        self.note_call();
        Ok((10, 20))
    }

    fn set_speed_limits(
        &self,
        _download_limit: u64,
        _upload_limit: u64,
    ) -> Result<(), DownloaderError> {
        self.note_call();
        Ok(())
    }
}

/// 不阻塞的默认下载器。
struct NoopDownloader;

impl Downloader for NoopDownloader {
    fn add(&self, _torrent: &Torrent) -> Result<(), DownloaderError> {
        Ok(())
    }

    fn completed_files(
        &self,
        _torrent: &Torrent,
    ) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        Ok(Vec::new())
    }

    fn get_speed_limits(&self) -> Result<(u64, u64), DownloaderError> {
        Ok((0, 0))
    }
}

struct NoopTransport;

impl NotifyTransport for NoopTransport {
    fn send(&self, _base: &str, _token: &str, _title: &str, _content: &str) -> Result<(), String> {
        Ok(())
    }
}

struct BlockingNotifyTransport {
    gate: Arc<Gate>,
}

impl NotifyTransport for BlockingNotifyTransport {
    fn send(&self, _base: &str, _token: &str, _title: &str, _content: &str) -> Result<(), String> {
        self.gate.enter_and_wait();
        Err("notify transport released by test gate".into())
    }
}

fn make_state(
    root: &Path,
    downloader: Arc<dyn Downloader>,
    transport: Arc<dyn NotifyTransport>,
    concurrency: usize,
) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        TOKEN.into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        downloader,
        root.join("library"),
    )
    .unwrap()
    .with_notify_transport(transport)
    .with_http_work(Arc::new(BlockingBudget::new(concurrency)))
}

fn configure_notify(state: &ApiState) {
    let store = state.store();
    let guard = store.lock();
    guard
        .put_setting("notify_url", "http://127.0.0.1:9/hook/system_alerts")
        .unwrap();
    guard
        .put_setting("notify_token", "notify-secret-token")
        .unwrap();
}

fn authed(builder: axum::http::request::Builder) -> axum::http::request::Builder {
    builder.header("authorization", format!("Bearer {TOKEN}"))
}

fn limits_request() -> Request<Body> {
    authed(Request::builder().method("GET").uri(LIMITS))
        .body(Body::empty())
        .unwrap()
}

fn put_limits_request() -> Request<Body> {
    authed(Request::builder().method("PUT").uri(LIMITS))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "download_limit_bytes": 100, "upload_limit_bytes": null }).to_string(),
        ))
        .unwrap()
}

fn notify_request() -> Request<Body> {
    authed(Request::builder().method("POST").uri(NOTIFY_TEST))
        .header("content-type", "application/json")
        .body(Body::from(json!({}).to_string()))
        .unwrap()
}

fn health_request() -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri("/api/v1/health")
        .body(Body::empty())
        .unwrap()
}

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// 发一个会阻塞在 downloader telemetry 的请求，并等它真的进入闭包。
async fn spawn_blocked_limits(
    app: &axum::Router,
    gate: &Arc<Gate>,
) -> tokio::task::JoinHandle<axum::response::Response> {
    let first = tokio::spawn({
        let app = app.clone();
        async move { app.oneshot(limits_request()).await.unwrap() }
    });
    gate.wait_entered();
    first
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn limits_capacity_exhaustion_returns_503() {
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::new());
    let state = make_state(
        tmp.path(),
        Arc::new(GatedDownloader::new(gate.clone())),
        Arc::new(NoopTransport),
        1,
    );
    let app = router(state);
    let first = spawn_blocked_limits(&app, &gate).await;
    let _release = ReleaseOnDrop(gate.clone());

    let second = tokio::time::timeout(
        Duration::from_secs(2),
        app.clone().oneshot(limits_request()),
    )
    .await
    .expect("busy rejection must not wait for the blocked worker")
    .unwrap();
    assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);

    gate.release();
    assert_eq!(first.await.unwrap().status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_limits_side_effect_uses_the_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::new());
    let state = make_state(
        tmp.path(),
        Arc::new(GatedDownloader::new(gate.clone())),
        Arc::new(NoopTransport),
        1,
    );
    let app = router(state);
    let first = spawn_blocked_limits(&app, &gate).await;
    let _release = ReleaseOnDrop(gate.clone());

    let second = tokio::time::timeout(
        Duration::from_secs(2),
        app.clone().oneshot(put_limits_request()),
    )
    .await
    .expect("side-effect action must not bypass the blocking budget")
    .unwrap();
    assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);

    gate.release();
    assert_eq!(first.await.unwrap().status(), StatusCode::OK);
}

#[tokio::test(flavor = "current_thread")]
async fn blocked_telemetry_runs_off_async_worker_and_health_stays_responsive() {
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::new());
    let downloader = Arc::new(GatedDownloader::new(gate.clone()));
    let state = make_state(tmp.path(), downloader.clone(), Arc::new(NoopTransport), 8);
    let app = router(state);
    let target = tokio::spawn({
        let app = app.clone();
        async move { app.oneshot(limits_request()).await.unwrap() }
    });
    let _release = ReleaseOnDrop(gate.clone());

    // 轮询 /health：只有 target 已经进入阻塞闭包后，health 仍然必须完成。
    let responsive = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if gate.entered() {
                let response = app.clone().oneshot(health_request()).await.unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "/health must complete while the downloader call blocks"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(
        responsive.is_ok(),
        "target never reached the blocking downloader call"
    );

    let worker_thread = std::thread::current().id();
    let call_thread = downloader.call_thread().expect("telemetry must have run");
    assert_ne!(
        call_thread, worker_thread,
        "blocking downloader call must not run on the async worker"
    );

    gate.release();
    assert_eq!(target.await.unwrap().status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notify_capacity_exhaustion_returns_503() {
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::new());
    let state = make_state(
        tmp.path(),
        Arc::new(NoopDownloader),
        Arc::new(BlockingNotifyTransport { gate: gate.clone() }),
        1,
    );
    configure_notify(&state);
    let app = router(state);
    let first = tokio::spawn({
        let app = app.clone();
        async move { app.oneshot(notify_request()).await.unwrap() }
    });
    gate.wait_entered();
    let _release = ReleaseOnDrop(gate.clone());

    let second = tokio::time::timeout(
        Duration::from_secs(2),
        app.clone().oneshot(notify_request()),
    )
    .await
    .expect("notify must use the blocking budget")
    .unwrap();
    assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);

    gate.release();
    let first = first.await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let body = json_body(first).await;
    assert_eq!(body["data"]["ok"], false);
    assert_eq!(body["data"]["sent"], false);
    assert!(body["data"]["error"].is_string());
}

#[tokio::test]
async fn notify_test_success_keeps_diagnostic_envelope() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(
        tmp.path(),
        Arc::new(NoopDownloader),
        Arc::new(NoopTransport),
        8,
    );
    configure_notify(&state);
    let app = router(state);

    let response = app.oneshot(notify_request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["data"]["ok"], true);
    assert_eq!(body["data"]["sent"], true);
}

#[tokio::test]
async fn notify_test_unconfigured_returns_400() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(
        tmp.path(),
        Arc::new(NoopDownloader),
        Arc::new(NoopTransport),
        8,
    );
    let app = router(state);

    let response = app.oneshot(notify_request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn unknown_downloader_id_is_bad_gateway() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(
        tmp.path(),
        Arc::new(NoopDownloader),
        Arc::new(NoopTransport),
        8,
    );
    let app = router(state);

    let uri = format!("{LIMITS}?id={}", uuid::Uuid::new_v4());
    let response = app
        .oneshot(
            authed(Request::builder().method("GET").uri(uri))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn invalid_downloader_id_is_bad_request() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(
        tmp.path(),
        Arc::new(NoopDownloader),
        Arc::new(NoopTransport),
        8,
    );
    let app = router(state);

    let response = app
        .oneshot(
            authed(
                Request::builder()
                    .method("GET")
                    .uri(format!("{LIMITS}?id=not-a-uuid")),
            )
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
