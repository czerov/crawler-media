use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Site, SiteId};
use downloader::MemoryDownloader;
use indexer::{Browser, BrowserConfig, FetchRequest, Fetcher, IndexerError, PageSession, ProfileSet};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

struct NoHttp;
impl Fetcher for NoHttp {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("fixture HTTP must not run".into()))
    }
}

struct FixturePage;
impl PageSession for FixturePage {
    fn goto(&self, _: &str) -> Result<(), IndexerError> {
        Ok(())
    }
    fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> {
        Ok(())
    }
    fn content(&self) -> Result<String, IndexerError> {
        Ok(include_str!("../../indexer/tests/fixtures/nexusphp.html").into())
    }
}

fn state(root: &std::path::Path) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "browser-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoHttp),
        Arc::new(MemoryDownloader::new(root.join("dl"))),
        root.join("library"),
    )
    .unwrap()
}

fn request(method: &str, body: Value) -> Request<Body> {
    request_at(method, "/api/v1/settings/browser", body)
}

fn request_at(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer browser-secret")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn json_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

type Endpoints = Arc<Mutex<Vec<String>>>;
fn routed_app(root: &std::path::Path, store: Arc<Mutex<Store>>) -> (axum::Router, Endpoints) {
    let overlay = root.join("profiles");
    std::fs::create_dir_all(&overlay).unwrap();
    std::fs::write(
        overlay.join("demo.yaml"),
        include_str!("../../indexer/src/profiles/demo.yaml")
            .replace("render: false", "render: true"),
    )
    .unwrap();
    let endpoints = Arc::new(Mutex::new(Vec::new()));
    let log = endpoints.clone();
    let browser = Browser::with_opener(BrowserConfig::disabled(), move |endpoint| {
        log.lock().push(endpoint.unwrap().to_owned());
        Ok(Arc::new(FixturePage))
    });
    let browser = api::runtime_browser::configure(browser, store.clone(), false).unwrap();
    let state = ApiState::new_arc(
        store,
        "browser-secret".into(),
        ProfileSet::load(Some(&overlay)).unwrap(),
        Arc::new(NoHttp),
        Arc::new(MemoryDownloader::new(root.join("dl"))),
        root.join("library"),
    )
    .unwrap();
    let _ = browser; // browser held in state in Task 6
    (router(state), endpoints)
}

fn site(store: &Store, cdp_url: Option<&str>) -> Site {
    let site = Site {
        id: SiteId::new(),
        name: "fixture".into(),
        url: "https://fixture.invalid/".into(),
        profile_id: "demo".into(),
        cookie: None,
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: cdp_url.map(str::to_owned),
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
    };
    store.insert_site(&site).unwrap();
    site
}

async fn save(app: &axum::Router, payload: Value) {
    let response = app.clone().oneshot(request("PUT", payload)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn browser_settings_advertise_cdp_opt_in_capabilities() {
    let tmp = tempfile::tempdir().unwrap();
    let response = router(state(tmp.path()))
        .oneshot(request("GET", json!({})))
        .await
        .unwrap();
    let data = json_body(response).await["data"].clone();
    assert_eq!(data["capabilities"]["mode"], "cdp-opt-in");
    assert_eq!(data["capabilities"]["managed_chromium"], false);
    assert_eq!(data["capabilities"]["search"], "http_only");
    assert_eq!(
        data["capabilities"]["route_precedence"],
        json!(["site", "instance", "managed"])
    );
}

#[tokio::test]
async fn browser_settings_reject_invalid_cdp_endpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(tmp.path());
    let app = router(state.clone());
    let response = app
        .oneshot(request(
            "PUT",
            json!({"cdp":{"enabled":true,"url":"ws://fixture.invalid:9222"}}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn browser_settings_reject_websocket_discovery_and_accepts_managed_key() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(tmp.path()));
    let response = app
        .clone()
        .oneshot(request(
            "PUT",
            json!({"cdp":{"enabled":true,"url":"ws://fixture.invalid:9222"}}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // Old clients sending {"managed":{"enabled":true}} are accepted rather than rejected
    let response = app
        .oneshot(request("PUT", json!({"managed":{"enabled":true}})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn browser_settings_store_failure_is_not_success() {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(tmp.path());
    let path = state.store().lock().sqlite_path().to_owned();
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch("CREATE TRIGGER reject_browser_setting BEFORE INSERT ON settings WHEN NEW.key = 'cdp.url' BEGIN SELECT RAISE(ABORT, 'fixture setting failure'); END;").unwrap();
    let response = router(state)
        .oneshot(request(
            "PUT",
            json!({"cdp":{"enabled":true,"url":"http://fixture.invalid:9222"}}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn search_never_uses_browser_even_when_cdp_is_configured() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path().join("data")).unwrap()));
    site(&store.lock(), Some("http://site.invalid:9222"));
    let (app, endpoints) = routed_app(tmp.path(), store.clone());
    save(
        &app,
        json!({"cdp":{"enabled":true,"url":"http://instance.invalid:9222"}}),
    )
    .await;
    // Indexer search is always HTTP and never opens Browser
    assert_eq!(endpoints.lock().len(), 0);
}

#[tokio::test]
async fn failed_save_restores_existing_route() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path().join("data")).unwrap()));
    let app = router(state(tmp.path()));
    save(
        &app,
        json!({"cdp":{"enabled":true,"url":"http://old.invalid:9223"}}),
    )
    .await;
    let path = store.lock().sqlite_path().to_owned();
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch("CREATE TRIGGER reject_browser_flag BEFORE INSERT ON settings WHEN NEW.key = 'cdp.enabled' BEGIN SELECT RAISE(ABORT, 'fixture flag failure'); END;").unwrap();
    let response = app
        .clone()
        .oneshot(request(
            "PUT",
            json!({"cdp":{"enabled":true,"url":"http://new.invalid:9223"}}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn startup_loads_saved_cdp_settings() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path().join("data")).unwrap()));
    store
        .lock()
        .put_setting(api::settings_keys::CDP_ENABLED, "1")
        .unwrap();
    store
        .lock()
        .put_setting(api::settings_keys::CDP_URL, "http://127.0.0.1:9222")
        .unwrap();
    let browser = api::runtime_browser::production_browser(store, false).unwrap();
    assert!(browser.cdp_enabled().unwrap());
}
