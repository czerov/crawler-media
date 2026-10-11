//! Filter (rule-set) mutation responses must report the same `is_default`
//! flag as a subsequent read. Regression for PATCH returning a hardcoded false.

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tower::ServiceExt;

#[path = "management/common.rs"]
mod common;
use common::{Fixtures, json_data, request, state};

fn test_app(tmp: &tempfile::TempDir) -> axum::Router {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    router(state(tmp.path(), fetcher, downloader))
}

fn filter_body(name: &str, keep_old_versions: Option<bool>) -> Value {
    let mut body = json!({
        "name": name,
        "atoms": [{ "kind": "resolution", "value": "1080p", "priority": 50 }],
    });
    if let Some(keep) = keep_old_versions {
        body["keep_old_versions"] = json!(keep);
    }
    body
}

async fn create_filter(app: &axum::Router, name: &str, keep_old_versions: Option<bool>) -> Value {
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/rule-sets",
            Some("management-secret"),
            filter_body(name, keep_old_versions),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    json_data(response).await
}

async fn set_default(app: &axum::Router, id: &Value) {
    let response = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/rule-sets/default",
            Some("management-secret"),
            json!({ "id": id }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_data(response).await;
    assert_eq!(body["default_rule_set_id"], *id);
}

async fn get_filter(app: &axum::Router, id: &str) -> Value {
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/rule-sets/{id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json_data(response).await
}

async fn patch_filter(app: &axum::Router, id: &str, name: &str, keep_old_versions: Option<bool>) -> Value {
    let response = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/rule-sets/{id}"),
            Some("management-secret"),
            filter_body(name, keep_old_versions),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json_data(response).await
}

#[tokio::test]
async fn patch_default_filter_returns_current_default_status() {
    let tmp = tempfile::tempdir().unwrap();
    let app = test_app(&tmp);
    let filter = create_filter(&app, "preferred", None).await;
    let id = filter["id"].as_str().unwrap().to_string();
    set_default(&app, &filter["id"]).await;

    let patched = patch_filter(&app, &id, "renamed", None).await;
    let reread = get_filter(&app, &id).await;

    assert_eq!(
        patched["is_default"], reread["is_default"],
        "PATCH response must agree with GET: patched={patched} reread={reread}"
    );
    assert_eq!(patched["is_default"], true);
    assert_eq!(patched["name"], "renamed");
    assert_eq!(reread["name"], "renamed");
}

#[tokio::test]
async fn patch_non_default_filter_reports_false() {
    let tmp = tempfile::tempdir().unwrap();
    let app = test_app(&tmp);
    let default = create_filter(&app, "preferred", None).await;
    set_default(&app, &default["id"]).await;
    let other = create_filter(&app, "other", None).await;
    let id = other["id"].as_str().unwrap().to_string();

    let patched = patch_filter(&app, &id, "other-renamed", None).await;

    assert_eq!(patched["is_default"], false);
    assert_eq!(get_filter(&app, &id).await["is_default"], false);
}

#[tokio::test]
async fn patch_preserves_keep_old_versions_when_omitted() {
    let tmp = tempfile::tempdir().unwrap();
    let app = test_app(&tmp);
    let filter = create_filter(&app, "keep", Some(true)).await;
    let id = filter["id"].as_str().unwrap().to_string();
    assert_eq!(filter["keep_old_versions"], true);

    let patched = patch_filter(&app, &id, "keep-renamed", None).await;

    assert_eq!(patched["keep_old_versions"], true);
    assert_eq!(get_filter(&app, &id).await["keep_old_versions"], true);
}
