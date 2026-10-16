use axum::body::Body;
use axum::http::{Request, StatusCode};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::tempdir;
use tower::ServiceExt;

use crate::common::{Fixtures, state};
use api::router;

#[tokio::test]
async fn test_browser_settings_crud_and_cdp_disabled_guard() {
    let tmp = tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("dl")));
    let state = state(tmp.path(), fetcher, downloader);
    let app = router(state);

    // 1. Initial GET /api/v1/settings/browser should have disabled defaults
    let req = Request::builder()
        .uri("/api/v1/settings/browser")
        .header("Authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 2. Sync CDP when disabled should return 400 Bad Request
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/settings/browser/sync-cdp")
        .header("Authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 3. PUT to update settings
    let update_body = serde_json::json!({
        "cdp": {
            "enabled": true,
            "url": "http://127.0.0.1:9999"
        },
        "obscura": {
            "enabled": true,
            "url": "http://127.0.0.1:9998"
        }
    });
    let req = Request::builder()
        .method("PUT")
        .uri("/api/v1/settings/browser")
        .header("Authorization", "Bearer management-secret")
        .header("Content-Type", "application/json")
        .body(Body::from(update_body.to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn browser_settings_reject_ws_credentials_and_query_endpoints() {
    let tmp = tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("dl")));
    let state = state(tmp.path(), fetcher, downloader);
    let app = router(state);

    for bad_url in [
        "ws://127.0.0.1:9222",
        "wss://127.0.0.1:9222",
        "http://user:pass@127.0.0.1:9222",
        "http://127.0.0.1:9222?token=xyz",
        "https://127.0.0.1:9222",
    ] {
        let req = Request::builder()
            .method("PUT")
            .uri("/api/v1/settings/browser")
            .header("Authorization", "Bearer management-secret")
            .header("Content-Type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "cdp": { "enabled": true, "url": bad_url }
                })
                .to_string(),
            ))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = crate::common::json_body(resp).await;
        assert_eq!(body["error"]["code"], "browser.invalid_endpoint");
    }
}

async fn create_and_login_member(app: &axum::Router, username: &str, password: &str) -> String {
    let create_res = app
        .clone()
        .oneshot(crate::common::request(
            "POST",
            "/api/v1/users",
            Some("management-secret"),
            serde_json::json!({
                "login": username,
                "password": password,
                "role": "member",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(create_res.status(), StatusCode::CREATED);

    let login_res = app
        .clone()
        .oneshot(crate::common::request(
            "POST",
            "/api/v1/auth/login",
            None,
            serde_json::json!({
                "username": username,
                "password": password,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(login_res.status(), StatusCode::OK);
    crate::common::json_body(login_res).await["data"]["token"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn member_cannot_modify_browser_settings_or_sync_cdp() {
    let tmp = tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("dl")));
    let state = state(tmp.path(), fetcher, downloader);
    let app = router(state);

    let member_token = create_and_login_member(&app, "alice", "alice-password").await;

    // 2. 普通成员尝试 PUT /api/v1/settings/browser：必须返回 403 Forbidden
    let req = Request::builder()
        .method("PUT")
        .uri("/api/v1/settings/browser")
        .header("Authorization", format!("Bearer {member_token}"))
        .header("Content-Type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "user_agent": "HackedUA/1.0"
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "普通成员禁止修改全局浏览器配置"
    );

    // 3. 普通成员尝试 POST /api/v1/settings/browser/sync-cdp：必须返回 403 Forbidden
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/settings/browser/sync-cdp")
        .header("Authorization", format!("Bearer {member_token}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "普通成员禁止触发全局 CDP 同步"
    );

    // 4. 普通成员尝试 POST /api/v1/settings/browser/obscura/start：必须返回 403 Forbidden
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/settings/browser/obscura/start")
        .header("Authorization", format!("Bearer {member_token}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // 5. 普通成员不能读取 Browser 设置/Obscura 状态（含端口、路径、CDP URL）
    for uri in ["/api/v1/settings/browser/obscura", "/api/v1/settings/browser"] {
        let req = Request::builder()
            .method("GET")
            .uri(uri)
            .header("Authorization", format!("Bearer {member_token}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{uri}");
    }
}

#[tokio::test]
async fn obscura_status_and_admin_start_stop() {
    let tmp = tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("dl")));
    let state = state(tmp.path(), fetcher, downloader);
    let app = router(state);

    // GET status
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/settings/browser/obscura")
        .header("Authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = crate::common::json_body(resp).await;
    assert_eq!(body["data"]["expected_version"], "v0.2.3");

    // POST stop
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/settings/browser/obscura/stop")
        .header("Authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn saving_browser_settings_reconciles_managed_obscura() {
    let tmp = tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("dl")));
    let state = state(tmp.path(), fetcher, downloader);
    let app = router(state);

    // 1. Configure CDP enabled with external endpoint -> Obscura not started, reason external_endpoint_configured
    let req = Request::builder()
        .method("PUT")
        .uri("/api/v1/settings/browser")
        .header("Authorization", "Bearer management-secret")
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::json!({
            "cdp": { "enabled": true, "url": "http://127.0.0.1:9222" }
        }).to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/settings/browser/obscura")
        .header("Authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let body = crate::common::json_body(resp).await;
    assert_eq!(body["data"]["not_started_reason"], "instance_endpoint_configured");

    // 2. Configure CDP disabled -> Obscura stopped, reason cdp_disabled
    let req = Request::builder()
        .method("PUT")
        .uri("/api/v1/settings/browser")
        .header("Authorization", "Bearer management-secret")
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::json!({
            "cdp": { "enabled": false, "url": "" }
        }).to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/settings/browser/obscura")
        .header("Authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let body = crate::common::json_body(resp).await;
    assert_eq!(body["data"]["not_started_reason"], "switch_off");

    // 3. Switch on, no instance endpoint, but no Browser Site needs the managed daemon.
    let req = Request::builder()
        .method("PUT")
        .uri("/api/v1/settings/browser")
        .header("Authorization", "Bearer management-secret")
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::json!({
            "cdp": { "enabled": true, "url": "" }
        }).to_string()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/settings/browser/obscura")
        .header("Authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let body = crate::common::json_body(resp).await;
    assert_eq!(body["data"]["not_started_reason"], "no_managed_browser_sites");
}

#[tokio::test]
async fn sync_cdp_errors_when_enabled_without_endpoint() {
    let tmp = tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("dl")));
    let state = state(tmp.path(), fetcher, downloader);
    let app = router(state);

    // Enable CDP with empty URL (and Obscura not running)
    let req = Request::builder()
        .method("PUT")
        .uri("/api/v1/settings/browser")
        .header("Authorization", "Bearer management-secret")
        .header("Content-Type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "cdp": { "enabled": true, "url": "" }
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Call sync-cdp -> returns 400 cdp.no_endpoint
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/settings/browser/sync-cdp")
        .header("Authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = crate::common::json_body(resp).await;
    assert_eq!(body["error"]["code"], "cdp.no_endpoint");
}
