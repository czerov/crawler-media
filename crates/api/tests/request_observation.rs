//! C5a：HTTP 请求观测（X-Request-Id、route 模板、延迟级别）与日志导出流式化。
//!
//! 纯函数 `observe` / `redact_query` 是 `pub(crate)`（见
//! `crates/api/src/http/request_observation.rs`），其单测放在模块内；
//! 本文件覆盖 HTTP 层与日志导出。

use std::path::Path;
use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use tower::ServiceExt;

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

fn app(root: &Path) -> axum::Router {
    router(state(root))
}

fn state(root: &Path) -> ApiState {
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

fn request(method: &str, uri: &str, token: Option<&str>, body: Value) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

async fn body_bytes(response: axum::response::Response) -> Vec<u8> {
    to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

#[tokio::test]
async fn response_carries_a_generated_request_id() {
    let tmp = tempfile::tempdir().unwrap();
    let response = app(tmp.path())
        .oneshot(request("GET", "/api/v1/health", None, Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let id = response
        .headers()
        .get("x-request-id")
        .expect("响应必须带 X-Request-Id")
        .to_str()
        .unwrap()
        .to_string();
    assert!(!id.is_empty());
    assert!(id.len() <= 64, "生成的 id 必须短小: {id}");
}

#[tokio::test]
async fn supplied_request_id_is_echoed_verbatim() {
    let tmp = tempfile::tempdir().unwrap();
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/health")
        .header("x-request-id", "trace-abc-123")
        .body(Body::empty())
        .unwrap();
    let response = app(tmp.path()).oneshot(request).await.unwrap();
    assert_eq!(
        response.headers().get("x-request-id").unwrap(),
        "trace-abc-123"
    );
}

#[tokio::test]
async fn unsafe_request_id_is_replaced_not_echoed() {
    let tmp = tempfile::tempdir().unwrap();
    // 含空格/非 ASCII 的 id 不能回写；尤其不能把 Authorization token 当 id。
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/health")
        .header("x-request-id", "bad id with spaces")
        .body(Body::empty())
        .unwrap();
    let response = app(tmp.path()).oneshot(request).await.unwrap();
    let id = response
        .headers()
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap();
    assert_ne!(id, "bad id with spaces");
    assert!(!id.contains(' '));
}

#[tokio::test]
async fn export_returns_the_latest_log_file_byte_for_byte_with_headers() {
    let tmp = tempfile::tempdir().unwrap();
    let logs_dir = tmp.path().join("data/logs");
    std::fs::create_dir_all(&logs_dir).unwrap();
    let payload = b"2026-10-12 line one\n2026-10-12 line two\n";
    std::fs::write(logs_dir.join("crawler-media.log.2026-10-12"), b"older\n").unwrap();
    std::fs::write(logs_dir.join("crawler-media.log.2026-10-13"), payload).unwrap();

    let response = app(tmp.path())
        .oneshot(request(
            "GET",
            "/api/v1/system/logs/export",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/plain; charset=utf-8"
    );
    assert!(response
        .headers()
        .get("content-disposition")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("attachment; filename=\"crawler-media-logs-"));
    assert_eq!(
        response
            .headers()
            .get("content-length")
            .expect("流式导出必须给出 Content-Length")
            .to_str()
            .unwrap(),
        payload.len().to_string()
    );
    assert_eq!(body_bytes(response).await, payload);
}

#[tokio::test]
async fn export_falls_back_to_the_memory_buffer_when_the_logs_dir_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let response = app(tmp.path())
        .oneshot(request(
            "GET",
            "/api/v1/system/logs/export",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/plain; charset=utf-8"
    );
    // 回退到内存缓冲：内容可以为空，但仍是 text/plain 附件。
    let _ = body_bytes(response).await;
}

#[tokio::test]
async fn sites_route_is_logged_with_the_matched_template() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(tmp.path());
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/sites",
            Some("management-secret"),
            json!({
                "name": "demo",
                "url": "https://pt.example/",
                "profile_id": "demo",
                "cookie": "uid=1",
                "enabled": true,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let site_id = serde_json::from_slice::<Value>(&body_bytes(created).await).unwrap()["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let captured = capture_logs(|| async {
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/sites/{site_id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    })
    .await;

    assert!(
        captured.contains("/sites/{id}"),
        "日志必须使用 MatchedPath 模板: {captured}"
    );
    assert!(
        !captured.contains(&site_id),
        "raw path 的 ID 不应进入日志维度: {captured}"
    );
    assert!(
        captured.contains("DEBUG"),
        "成功的普通请求应为 debug 级别: {captured}"
    );
}

#[tokio::test]
async fn matched_4xx_route_is_logged_at_warn() {
    let tmp = tempfile::tempdir().unwrap();
    // 已匹配路由返回 4xx（未匹配路径的 404 由 router fallback 产生，不经过本中间件）。
    let missing = uuid::Uuid::new_v4();
    let captured = capture_logs(|| async {
        let response = app(tmp.path())
            .oneshot(request(
                "GET",
                &format!("/api/v1/sites/{missing}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    })
    .await;
    assert!(captured.contains("WARN"), "4xx 应为 warn: {captured}");
}

/// 线程内捕获日志文本（不抢全局 subscriber，不断言日志总数）。
async fn capture_logs<F, Fut>(run: F) -> String
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    use std::io::Write;
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);
    struct Writer(Arc<Mutex<Vec<u8>>>);
    impl Write for Writer {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buffer {
        type Writer = Writer;
        fn make_writer(&'a self) -> Writer {
            Writer(self.0.clone())
        }
    }

    let buffer = Buffer::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buffer.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    run().await;
    drop(guard);
    let bytes = buffer.0.lock().unwrap().clone();
    String::from_utf8_lossy(&bytes).to_string()
}

#[test]
fn slow_request_env_parsing_is_checked_u64() {
    use api::config::{DEFAULT_SLOW_REQUEST_MS, parse_slow_request_ms};
    assert_eq!(parse_slow_request_ms(Some("250")), 250);
    assert_eq!(parse_slow_request_ms(Some("0")), 0);
    assert_eq!(parse_slow_request_ms(Some(" 42 ")), 42);
    assert_eq!(parse_slow_request_ms(None), DEFAULT_SLOW_REQUEST_MS);
    assert_eq!(
        parse_slow_request_ms(Some("not-a-number")),
        DEFAULT_SLOW_REQUEST_MS
    );
    // checked u64：溢出 u64 的值必须回退默认，而不是截断/回绕。
    assert_eq!(
        parse_slow_request_ms(Some("99999999999999999999999999")),
        DEFAULT_SLOW_REQUEST_MS
    );
    assert_eq!(parse_slow_request_ms(Some("-1")), DEFAULT_SLOW_REQUEST_MS);
}

/// 用注入阈值的中间件构造最小 router（不触碰全局 env）。
fn observed_router(slow_after: std::time::Duration) -> axum::Router {
    use axum::routing::get;
    axum::Router::new()
        .route("/ok", get(|| async { "ok" }))
        .route("/boom", get(|| async { StatusCode::INTERNAL_SERVER_ERROR }))
        .route("/health", get(|| async { "ok" }))
        .route("/events/stream", get(|| async { "sse" }))
        .route(
            "/slow",
            get(|| async {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                "slow"
            }),
        )
        .layer(axum::middleware::from_fn_with_state(
            slow_after,
            api::http::request_logger,
        ))
}

fn plain_get(uri: &str) -> Request<Body> {
    Request::builder().method("GET").uri(uri).body(Body::empty()).unwrap()
}

#[tokio::test]
async fn server_error_is_logged_at_error() {
    let captured = capture_logs(|| async {
        let response = observed_router(std::time::Duration::from_secs(30))
            .oneshot(plain_get("/boom"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    })
    .await;
    assert!(captured.contains("ERROR"), "5xx 应为 error: {captured}");
}

#[tokio::test]
async fn slow_request_escalates_to_warn_and_zero_threshold_disables_it() {
    let captured = capture_logs(|| async {
        let response = observed_router(std::time::Duration::from_millis(1))
            .oneshot(plain_get("/slow"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    })
    .await;
    assert!(
        captured.contains("WARN") && captured.contains("HTTP 请求缓慢"),
        "超阈值应为 warn: {captured}"
    );

    let captured = capture_logs(|| async {
        let response = observed_router(std::time::Duration::ZERO)
            .oneshot(plain_get("/slow"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    })
    .await;
    assert!(
        !captured.contains("WARN"),
        "阈值 0 必须关闭升级: {captured}"
    );
    assert!(captured.contains("DEBUG"), "成功请求应为 debug: {captured}");
}

#[tokio::test]
async fn health_and_sse_are_excluded_from_observation() {
    let captured = capture_logs(|| async {
        for uri in ["/health", "/events/stream"] {
            let response = observed_router(std::time::Duration::from_millis(1))
                .oneshot(plain_get(uri))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
    })
    .await;
    assert!(
        !captured.contains("HTTP 请求"),
        "/health 与 SSE 不应产生观测日志: {captured}"
    );
}

#[tokio::test]
async fn observed_router_still_echoes_request_ids() {
    let request = Request::builder()
        .method("GET")
        .uri("/ok")
        .header("x-request-id", "req-1")
        .body(Body::empty())
        .unwrap();
    let response = observed_router(std::time::Duration::ZERO)
        .oneshot(request)
        .await
        .unwrap();
    assert_eq!(response.headers().get("x-request-id").unwrap(), "req-1");
}
