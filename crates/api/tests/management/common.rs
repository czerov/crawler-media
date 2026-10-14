use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use downloader::Downloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

pub(crate) struct Fixtures {
    pub requests: Mutex<Vec<String>>,
    pub bodies: HashMap<&'static str, &'static str>,
}

impl Fetcher for Fixtures {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        self.requests.lock().push(request.key.clone());
        let mode = request.key.split_once(':').unwrap().0;
        self.bodies
            .get(mode)
            .map(|body| (*body).to_string())
            .ok_or_else(|| IndexerError::Fetch(format!("no {mode} fixture")))
    }
}

pub(crate) fn request(method: &str, uri: &str, token: Option<&str>, body: Value) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    request.body(Body::from(body.to_string())).unwrap()
}

pub(crate) async fn json_body(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

pub(crate) async fn json_data(response: axum::response::Response) -> Value {
    let status = response.status();
    let body = json_body(response).await;
    assert_eq!(
        body["ok"], true,
        "expected v1 envelope ok=true, status={status} body={body}"
    );
    body.get("data").cloned().unwrap_or(Value::Null)
}

/// 自有 API 失败体。非 `{ok:false, error:{code,message}}` 时 panic。
pub(crate) async fn json_error(response: axum::response::Response) -> Value {
    let status = response.status();
    let body = json_body(response).await;
    assert_eq!(
        body["ok"], false,
        "expected v1 envelope ok=false, status={status} body={body}"
    );
    let error = body.get("error").cloned().unwrap_or(Value::Null);
    assert!(
        error.get("code").and_then(Value::as_str).is_some(),
        "missing error.code: {body}"
    );
    assert!(
        error.get("message").and_then(Value::as_str).is_some(),
        "missing error.message: {body}"
    );
    error
}

pub(crate) fn state(
    root: &Path,
    fetcher: Arc<Fixtures>,
    downloader: Arc<dyn Downloader>,
) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        fetcher,
        downloader,
        root.join("library"),
    )
    .unwrap()
}

pub(crate) fn site_payload() -> Value {
    json!({
        "name": "demo",
        "url": "https://pt.example/",
        "profile_id": "demo",
        "cookie": "uid=1; pass=abc",
        "rss_url": "https://pt.example/rss",
        "rate_limit_per_minute": 12,
        "enabled": true
    })
}

pub(crate) fn subscribe_payload(fetch_mode: &str) -> Value {
    json!({
        "media": { "kind": "movie", "title": "The Matrix" },
        "coverage": { "kind": "movie" },
        "fetch_mode": fetch_mode,
        "filter": {
            "name": "matrix-uhd",
            "atoms": [
                { "kind": "resolution", "value": "2160p", "priority": 100 },
                { "kind": "title_match", "value": "Matrix", "priority": 80 }
            ]
        },
        "wash_cut": false,
        "full_season_pack": false
    })
}

pub(crate) async fn create_site(app: &axum::Router) -> Value {
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/sites",
            Some("management-secret"),
            site_payload(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    json_data(response).await
}

pub(crate) async fn create_subscribe(app: &axum::Router, fetch_mode: &str) -> Value {
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            subscribe_payload(fetch_mode),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    json_data(response).await
}

pub(crate) async fn create_api_subscribe(app: &axum::Router, title: &str, token: &str) -> Value {
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some(token),
            json!({
                "media": { "kind": "movie", "title": title },
                "coverage": { "kind": "movie" },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    json_body(response).await["data"].clone()
}

pub(crate) async fn create_member(app: &axum::Router, login: &str) -> (domain::UserId, String) {
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/users",
            Some("management-secret"),
            json!({ "login": login, "password": "member-pass" }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let user_id =
        domain::UserId::from_str(json_body(created).await["data"]["id"].as_str().unwrap()).unwrap();
    let login_response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": login, "password": "member-pass" }),
        ))
        .await
        .unwrap();
    let token = json_body(login_response).await["data"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    (user_id, token)
}

pub(crate) fn nexusphp() -> &'static str {
    include_str!("../../../indexer/tests/fixtures/nexusphp.html")
}

pub(crate) fn rss_xml() -> &'static str {
    include_str!("../../../indexer/tests/fixtures/rss.xml")
}

pub(crate) async fn scan_and_wait(app: &axum::Router, lib_id: &str) -> Value {
    let scan = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{lib_id}/scan"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(scan.status(), StatusCode::ACCEPTED);
    let body = json_body(scan).await;
    let op_id = body["data"]["operation_id"].as_str().unwrap();
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let poll = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{lib_id}/scan-operations/{op_id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        if poll.status() == StatusCode::OK {
            let p_body = json_body(poll).await;
            if p_body["data"]["status"] == "completed" {
                return p_body["data"]["report"].clone();
            }
        }
    }
    panic!("scan operation timed out");
}
