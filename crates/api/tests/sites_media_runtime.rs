//! A5b runtime isolation regressions for Site / media / organize entries.
//!
//! 复现：Site verify 在 async worker 上直接跑同步 `Indexer::search`（ureq 阻塞
//! 1-8s）；组织整理/元数据刷新等同步用例同理。这些路径必须走
//! `ApiState::http_work` 的 owned blocking 闭包，`/health` 不被拖住。

use std::path::Path;
use std::sync::{Arc, Condvar, Mutex as StdMutex};
use std::thread::ThreadId;
use std::time::Duration;

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "management-secret";

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

/// Indexer 用的 Fetcher：阻塞在 gate 上并记录真实调用线程。
struct GatedFetcher {
    gate: Arc<Gate>,
    call_thread: StdMutex<Option<ThreadId>>,
}

impl GatedFetcher {
    fn new(gate: Arc<Gate>) -> Self {
        Self {
            gate,
            call_thread: StdMutex::new(None),
        }
    }

    fn call_thread(&self) -> Option<ThreadId> {
        *self.call_thread.lock().unwrap()
    }
}

impl Fetcher for GatedFetcher {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        *self.call_thread.lock().unwrap() = Some(std::thread::current().id());
        self.gate.enter_and_wait();
        Err(IndexerError::Fetch("gated fixture released".into()))
    }
}

fn state<F: Fetcher + 'static>(root: &Path, fetcher: Arc<F>) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        TOKEN.into(),
        ProfileSet::load(None).unwrap(),
        fetcher,
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

async fn create_site(
    app: &axum::Router,
    profile_id: &str,
    url: &str,
    cookie: Option<&str>,
) -> domain::SiteId {
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/sites",
            Some(TOKEN),
            json!({
                "name": profile_id,
                "url": url,
                "profile_id": profile_id,
                "cookie": cookie,
                "enabled": true,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = json_body(response).await;
    body["data"]["id"].as_str().unwrap().parse().unwrap()
}

async fn site_action(app: &axum::Router, id: &str, action: &str) -> axum::response::Response {
    app.clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/sites/{id}/{action}"),
            Some(TOKEN),
            Value::Null,
        ))
        .await
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn blocked_site_verify_keeps_health_responsive() {
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::new());
    let fetcher = Arc::new(GatedFetcher::new(gate.clone()));
    let state = state(tmp.path(), fetcher.clone());
    let app = router(state);
    let id = create_site(&app, "pterclub", "http://127.0.0.1:9/", Some("uid=1; pass=abc")).await;

    let target = tokio::spawn({
        let app = app.clone();
        async move { site_action(&app, &id.to_string(), "verify").await }
    });
    let _release = ReleaseOnDrop(gate.clone());

    // 独立 OS watchdog：即使 verify 在 async worker 上同步阻塞（回归时），
    // 也会放行 gate，让测试以断言失败收场而不是整条 runtime 死锁。
    let released = Arc::new(StdMutex::new(false));
    std::thread::spawn({
        let gate = gate.clone();
        let released = released.clone();
        move || {
            std::thread::sleep(Duration::from_secs(8));
            *released.lock().unwrap() = true;
            gate.release();
        }
    });

    // 只有 verify 真的进入 blocking 闭包后，/health 才必须仍能完成。
    let responsive = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            if gate.entered() {
                let response = app.clone().oneshot(health_request()).await.unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "/health must complete while Site verify blocks"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(
        responsive.is_ok(),
        "verify never reached the blocking fetcher"
    );
    assert!(
        !*released.lock().unwrap(),
        "blocked Site verify stalled the async worker until the watchdog released it"
    );

    let worker_thread = std::thread::current().id();
    assert_ne!(
        fetcher.call_thread().expect("search must have run"),
        worker_thread,
        "Site verify must not run its blocking search on the async worker"
    );

    gate.release();
    let response = target.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"]["ok"], false, "gated upstream must report failure");
}

#[tokio::test]
async fn site_action_status_contracts_are_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(tmp.path(), Arc::new(NoFetch)));

    let missing = domain::SiteId::new();
    for action in ["verify", "login", "check-in"] {
        let response = site_action(&app, &missing.to_string(), action).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{action}");
        assert_eq!(json_body(response).await["error"]["code"], "site.missing");
    }

    let invalid = site_action(&app, "not-a-site-id", "verify").await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_body(invalid).await["error"]["code"], "site.invalid");

    // API 模板站点不支持页面 Login/Check-in。
    let api_site = create_site(&app, "mteam", "https://api.m-team.cc/", None).await;
    for action in ["login", "check-in"] {
        let response = site_action(&app, &api_site.to_string(), action).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{action}");
        assert_eq!(
            json_body(response).await["error"]["code"],
            "site.maintenance_unsupported"
        );
    }

    // NexusPHP 站点无 cookie 且上游不可达 → 原有 login_failed 契约（仅 loopback）。
    let nexus = create_site(&app, "pterclub", "http://127.0.0.1:9/", None).await;
    let response = site_action(&app, &nexus.to_string(), "login").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_body(response).await["error"]["code"], "site.login_failed");
}

fn ledger_row(media_id: MediaId, path: &Path) -> LedgerRow {
    LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: path.display().to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    }
}

#[tokio::test]
async fn organize_side_effect_really_renames_on_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(tmp.path(), Arc::new(NoFetch));
    let root = tmp.path().join("organized");
    std::fs::create_dir_all(&root).unwrap();
    let library = {
        let store = state.store();
        let store = store.lock();
        store
            .create_library(
                MediaKind::Movie,
                "Organize",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
    };
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Rename Me".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let old = root.join("old-name.mkv");
    let new = root.join("new-name.mkv");
    std::fs::write(&old, b"payload").unwrap();
    {
        let store = state.store();
        let store = store.lock();
        store.insert_media(&media).unwrap();
        store.insert_ledger(&ledger_row(media.id, &old)).unwrap();
    }

    let app = router(state.clone());
    let response = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{}/organize", library.id),
            Some(TOKEN),
            json!({ "renames": [{ "from": old.display().to_string(), "to": new.display().to_string() }] }),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"]["applied"], 1, "body={body}");
    assert!(new.is_file(), "side-effect rename must have really executed");
    assert!(!old.exists(), "source file must be gone after organize");
    let store = state.store();
    let store = store.lock();
    assert!(
        store
            .ledger_by_path(&new.display().to_string())
            .unwrap()
            .is_some(),
        "ledger must follow the renamed file"
    );
}
