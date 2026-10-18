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
    // 扫描是异步 coordinator 作业；全量并行跑测试时（workspace 一次拉起上百个二进制）
    // 5 秒预算会被别的测试抢占 CPU 而误判超时，这里放宽到 30 秒。
    for _ in 0..600 {
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

/// Install a deterministic fake `ffprobe` first on PATH (once per test binary).
/// A file whose content starts with `PROBE:` yields the JSON after the prefix;
/// anything else fails like ffprobe on invalid data (the probe-failure branch).
pub(crate) fn install_fake_ffprobe() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("crawler-media-e2e-bin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("ffprobe");
        // Write under a temp name and rename into place so a concurrent
        // fork/exec never sees the file open for writing (ETXTBSY).
        let staging = dir.join("ffprobe.tmp");
        std::fs::write(
            &staging,
            "#!/bin/sh\nfor last; do :; done\nif grep -q SLOWPROBE \"$last\" 2>/dev/null; then sleep 2; fi\nif [ \"$(head -c 6 \"$last\" 2>/dev/null)\" = \"PROBE:\" ]; then tail -c +7 \"$last\"; exit 0; fi\necho 'Invalid data found when processing input' >&2\nexit 1\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::rename(&staging, &script).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        // SAFETY: test-only, executed once before the harness spawns workers.
        unsafe { std::env::set_var("PATH", format!("{}:{path}", dir.display())) };
    });
}

/// Synthetic media bytes that the fake ffprobe reports with this quality.
pub(crate) fn probed_video(height: u32, codec: &str, hdr: bool) -> Vec<u8> {
    let width = match height {
        2160 => 3840,
        1080 => 1920,
        720 => 1280,
        _ => 640,
    };
    let transfer = if hdr {
        r#","color_transfer":"smpte2084""#
    } else {
        ""
    };
    format!(r#"PROBE:{{"streams":[{{"codec_name":"{codec}","width":{width},"height":{height}{transfer}}}]}}"#)
        .into_bytes()
}

/// Bytes the fake ffprobe cannot read (probe failure).
pub(crate) fn unprobeable_video() -> Vec<u8> {
    b"not-a-real-video".to_vec()
}
