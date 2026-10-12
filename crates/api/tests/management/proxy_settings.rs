//! Tests for metadata network proxy settings and probe endpoint.

use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

static PROXY_TEST_MUTEX: Mutex<()> = Mutex::new(());

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    router(state(tmp.path(), fetcher, downloader))
}

#[tokio::test]
async fn proxy_settings_default_and_round_trip() {
    let _guard = PROXY_TEST_MUTEX.lock();
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    // 1. Initial read
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/settings/proxy",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let data = &body["data"];
    assert_eq!(data["proxy_url"], "");
    assert_eq!(data["douban_bypass"], true);

    // 2. Put socks5 proxy
    let response = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/settings/proxy",
            Some("management-secret"),
            json!({
                "proxy_url": "socks5://127.0.0.1:1080",
                "douban_bypass": false,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let data = &body["data"];
    assert_eq!(data["proxy_url"], "socks5://127.0.0.1:1080");
    assert_eq!(data["douban_bypass"], false);
    assert_eq!(data["active_proxy"], "socks5://127.0.0.1:1080");

    // 3. Verify hot-sync in memory
    assert_eq!(
        api::http_agent::current_proxy(),
        Some("socks5://127.0.0.1:1080".into())
    );
    assert_eq!(api::http_agent::douban_bypass(), false);

    // 4. Clear proxy
    let response = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/settings/proxy",
            Some("management-secret"),
            json!({
                "proxy_url": "",
                "douban_bypass": true,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(api::http_agent::current_proxy(), None);
    assert_eq!(api::http_agent::douban_bypass(), true);
}

#[tokio::test]
async fn proxy_settings_with_auth_and_masking() {
    let _guard = PROXY_TEST_MUTEX.lock();
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    // 1. Put proxy with separate username and password
    let response = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/settings/proxy",
            Some("management-secret"),
            json!({
                "proxy_url": "http://127.0.0.1:8888",
                "username": "myadmin",
                "password": "supersecretpassword",
                "douban_bypass": true,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let data = &body["data"];
    assert_eq!(data["proxy_url"], "http://127.0.0.1:8888");
    assert_eq!(data["username"], "myadmin");
    assert_eq!(data["has_password"], true);
    // Active proxy in response must mask credentials
    assert_eq!(data["active_proxy"], "http://myadmin:***@127.0.0.1:8888");

    // 2. Underlying http_agent current_proxy has full credentials
    assert_eq!(
        api::http_agent::current_proxy(),
        Some("http://myadmin:supersecretpassword@127.0.0.1:8888".into())
    );

    // 3. GET response masks password
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/settings/proxy",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let data = &body["data"];
    assert_eq!(data["username"], "myadmin");
    assert_eq!(data["has_password"], true);
    assert_eq!(data["active_proxy"], "http://myadmin:***@127.0.0.1:8888");
}

/// 没有 scheme 的 `user:pass@host` 也必须脱敏。
///
/// 旧实现（`http/proxy_settings.rs` 里的本地副本）只在含 `://` 时才脱敏，遇到
/// 无 scheme 的输入会**原样返回**——`GET/PUT /settings/proxy` 的 `active_proxy`
/// 就会把明文口令回给前端。这条回归钉住"与 diagnose/domain 同一套渲染"。
#[tokio::test]
async fn proxy_settings_mask_schemeless_credentials() {
    let _guard = PROXY_TEST_MUTEX.lock();
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let response = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/settings/proxy",
            Some("management-secret"),
            json!({
                "proxy_url": "127.0.0.1:8888",
                "username": "myadmin",
                "password": "supersecretpassword",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let put_active = body["data"]["active_proxy"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !put_active.contains("supersecretpassword"),
        "PUT 响应不得输出明文口令: {put_active}"
    );
    assert_eq!(put_active, "myadmin:***@127.0.0.1:8888");

    let response = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/settings/proxy",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let get_active = body["data"]["active_proxy"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !get_active.contains("supersecretpassword"),
        "GET 响应不得输出明文口令: {get_active}"
    );
    assert_eq!(get_active, "myadmin:***@127.0.0.1:8888");
}

#[test]
fn format_proxy_url_supports_embedded_and_separate_credentials() {
    use api::http_agent::format_proxy_url;

    // Direct without auth
    assert_eq!(
        format_proxy_url("socks5://127.0.0.1:1080", None, None),
        "socks5://127.0.0.1:1080"
    );

    // Separate credentials
    assert_eq!(
        format_proxy_url("socks5://127.0.0.1:1080", Some("user1"), Some("pass1")),
        "socks5://user1:pass1@127.0.0.1:1080"
    );

    // Username only
    assert_eq!(
        format_proxy_url("http://10.0.0.1:3128", Some("user1"), None),
        "http://user1@10.0.0.1:3128"
    );

    // Already embedded credentials preserved
    assert_eq!(
        format_proxy_url(
            "http://custom:secret@10.0.0.1:3128",
            Some("other"),
            Some("other")
        ),
        "http://custom:secret@10.0.0.1:3128"
    );
}

#[tokio::test]
async fn proxy_test_endpoint_probes_target() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    // Test with invalid proxy URL (should fail gracefully with error, not panic)
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/settings/proxy/test",
            Some("management-secret"),
            json!({
                "proxy_url": "http://127.0.0.1:65530", // dead port
                "target": "http://127.0.0.1:18765/api/v1/health",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let data = &body["data"];
    assert_eq!(data["ok"], false);
    assert!(data["error"].is_string());
}

#[tokio::test]
async fn proxy_diagnose_endpoint_lists_all_targets() {
    let _guard = PROXY_TEST_MUTEX.lock();
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/settings/proxy/diagnose",
            Some("management-secret"),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let data = &body["data"];
    assert_eq!(data["douban_bypass"], true);
    let items = data["items"].as_array().expect("items array");
    assert!(items.len() >= 5);
    // Ensure all major metadata domains are covered in diagnostics
    let names: Vec<&str> = items.iter().filter_map(|it| it["name"].as_str()).collect();
    assert!(names.iter().any(|n| n.contains("TMDB")));
    assert!(names.iter().any(|n| n.contains("Bangumi")));
    assert!(names.iter().any(|n| n.contains("豆瓣")));
}
