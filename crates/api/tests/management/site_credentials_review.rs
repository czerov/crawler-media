//! Role boundaries and lossless Site credential editing through HTTP.
use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tempfile::tempdir;
use tower::ServiceExt;

use super::common::{Fixtures, create_member, json_data, request, state};

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ))
}

fn payload() -> Value {
    json!({
        "name": "credential-site", "profile_id": "demo", "enabled": true,
        "url": "https://base-user:base-password@pt.example/base-path-secret?key=base-query-secret#base-fragment-secret",
        "rss_url": "https://pt.example/rss/rss-path-secret?passkey=rss-passkey-secret#rss-fragment-secret",
        "proxy": "http://proxy-user:proxy-password@proxy.example:8080/proxy-path-secret?token=proxy-query-secret",
        "cdp_url": "http://browser.example:9222/cdp-path-secret",
        "cookie": "cookie-secret", "api_key": "api-key-secret"
    })
}

async fn call(app: &axum::Router, method: &str, uri: &str, token: &str, body: Value) -> Value {
    let response = app
        .clone()
        .oneshot(request(method, uri, Some(token), body))
        .await
        .unwrap();
    let expected = if method == "POST" {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    assert_eq!(response.status(), expected);
    json_data(response).await
}

async fn create(app: &axum::Router) -> String {
    call(app, "POST", "/api/v1/sites", "management-secret", payload()).await["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn assert_admin_urls(row: &Value) {
    for key in ["url", "rss_url", "proxy", "cdp_url"] {
        assert_eq!(
            row[key],
            payload()[key],
            "administrator must receive original {key}"
        );
    }
    assert!(row["cookie"].is_null());
    assert!(row["api_key"].is_null());
    assert_eq!(row["auth_type"], "apikey");
}

fn assert_member_view(row: &Value) {
    let serialized = row.to_string();
    for secret in [
        "base-user",
        "base-password",
        "base-path-secret",
        "base-query-secret",
        "base-fragment-secret",
        "rss-passkey-secret",
        "rss-path-secret",
        "rss-fragment-secret",
        "proxy-user",
        "proxy-password",
        "proxy-path-secret",
        "proxy-query-secret",
        "cdp-user",
        "cdp-password",
        "cdp-path-secret",
        "cdp-query-secret",
        "cookie-secret",
        "api-key-secret",
    ] {
        assert!(
            !serialized.contains(secret),
            "member response leaked {secret}: {row}"
        );
    }
    assert_eq!(row["name"], "credential-site");
    assert_eq!(row["profile_id"], "demo");
    assert_eq!(row["enabled"], true);
    assert_eq!(row["auth_type"], "apikey");
}

#[tokio::test]
async fn member_list_and_detail_hide_all_credential_url_components() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let id = create(&app).await;
    let (_, member) = create_member(&app, "credential-member").await;
    let rows = call(&app, "GET", "/api/v1/sites", &member, Value::Null).await;
    assert_member_view(&rows[0]);
    let row = call(
        &app,
        "GET",
        &format!("/api/v1/sites/{id}"),
        &member,
        Value::Null,
    )
    .await;
    assert_member_view(&row);
}

#[tokio::test]
async fn admin_list_and_detail_keep_original_urls_but_mask_cookie_and_api_key() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let id = create(&app).await;
    let rows = call(
        &app,
        "GET",
        "/api/v1/sites",
        "management-secret",
        Value::Null,
    )
    .await;
    assert_admin_urls(&rows[0]);
    let row = call(
        &app,
        "GET",
        &format!("/api/v1/sites/{id}"),
        "management-secret",
        Value::Null,
    )
    .await;
    assert_admin_urls(&row);
}

#[tokio::test]
async fn admin_get_put_roundtrip_does_not_erase_masked_credentials() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let id = create(&app).await;
    let uri = format!("/api/v1/sites/{id}");
    let mut row = call(&app, "GET", &uri, "management-secret", Value::Null).await;
    row["name"] = json!("renamed");
    call(&app, "PUT", &uri, "management-secret", row).await;
    assert_persisted_credentials(&tmp, &id);
    let updated = call(&app, "GET", &uri, "management-secret", Value::Null).await;
    assert_eq!(updated["name"], "renamed");
    assert_admin_urls(&updated);
}

fn assert_persisted_credentials(tmp: &tempfile::TempDir, id: &str) {
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let site = store.get_site(id.parse().unwrap()).unwrap().unwrap();
    assert_eq!(site.cookie.as_deref(), Some("cookie-secret"));
    assert_eq!(site.api_key.as_deref(), Some("api-key-secret"));
    assert_eq!(site.url, payload()["url"].as_str().unwrap());
    assert_eq!(site.rss_url.as_deref(), payload()["rss_url"].as_str());
    assert_eq!(site.proxy.as_deref(), payload()["proxy"].as_str());
    assert_eq!(site.cdp_url.as_deref(), payload()["cdp_url"].as_str());
}

#[tokio::test]
async fn absent_and_null_credentials_preserve_originals_on_put_and_patch() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let id = create(&app).await;
    let uri = format!("/api/v1/sites/{id}");
    for method in ["PUT", "PATCH"] {
        for nulls in [false, true] {
            let mut body =
                json!({ "name": "edited", "url": payload()["url"], "profile_id": "demo" });
            if nulls {
                for key in ["cookie", "api_key", "rss_url", "proxy", "cdp_url"] {
                    body[key] = Value::Null;
                }
            }
            call(&app, method, &uri, "management-secret", body).await;
            assert_persisted_credentials(&tmp, &id);
            let row = call(&app, "GET", &uri, "management-secret", Value::Null).await;
            assert_admin_urls(&row);
        }
    }
}

#[tokio::test]
async fn explicit_url_replacements_and_empty_credential_clears_are_persisted() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let id = create(&app).await;
    let uri = format!("/api/v1/sites/{id}");
    let replacements = json!({
        "url": "https://replacement.example/", "rss_url": "https://replacement.example/rss?key=new",
        "proxy": "http://new:secret@proxy.example/", "cdp_url": "http://browser.example:9222/new"
    });
    call(
        &app,
        "PATCH",
        &uri,
        "management-secret",
        replacements.clone(),
    )
    .await;
    let row = call(&app, "GET", &uri, "management-secret", Value::Null).await;
    for key in ["url", "rss_url", "proxy", "cdp_url"] {
        assert_eq!(row[key], replacements[key]);
    }
    call(
        &app,
        "PATCH",
        &uri,
        "management-secret",
        json!({
            "cookie": "", "api_key": "", "rss_url": "", "proxy": "", "cdp_url": ""
        }),
    )
    .await;
    let row = call(&app, "GET", &uri, "management-secret", Value::Null).await;
    for key in ["cookie", "api_key", "rss_url", "proxy", "cdp_url"] {
        assert!(row[key].is_null());
    }
    assert_eq!(row["auth_type"], "none");
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let site = store.get_site(id.parse().unwrap()).unwrap().unwrap();
    assert!(site.cookie.is_none() && site.api_key.is_none());
}

#[tokio::test]
async fn site_maintenance_via_round_trips_and_rejects_unknown_values() {
    let tmp = tempdir().unwrap();
    let app = app(&tmp);
    let mut p = payload();
    p["maintenance_via"] = json!("browser");
    p["proxy"] = json!(""); // browser cannot have proxy
    let res = call(&app, "POST", "/api/v1/sites", "management-secret", p.clone()).await;
    assert_eq!(res["maintenance_via"], "browser");

    p["maintenance_via"] = json!("invalid_via");
    let resp = app.clone().oneshot(request("POST", "/api/v1/sites", Some("management-secret"), p)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn site_with_proxy_cannot_choose_browser_maintenance() {
    let tmp = tempdir().unwrap();
    let app = app(&tmp);
    let mut p = payload();
    p["maintenance_via"] = json!("browser");
    p["proxy"] = json!("http://proxy.example:8080");
    let resp = app.clone().oneshot(request("POST", "/api/v1/sites", Some("management-secret"), p)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn browser_site_cannot_add_a_proxy() {
    let tmp = tempdir().unwrap();
    let app = app(&tmp);
    let mut p = payload();
    p["maintenance_via"] = json!("browser");
    p["proxy"] = json!("");
    let site = call(&app, "POST", "/api/v1/sites", "management-secret", p).await;
    let site_id = site["id"].as_str().unwrap();

    let patch_resp = app.clone().oneshot(request("PATCH", &format!("/api/v1/sites/{site_id}"), Some("management-secret"), json!({"proxy": "http://proxy.example:8080"}))).await.unwrap();
    assert_eq!(patch_resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn switching_back_to_http_and_adding_a_proxy_in_one_patch_is_allowed() {
    let tmp = tempdir().unwrap();
    let app = app(&tmp);
    let mut p = payload();
    p["maintenance_via"] = json!("browser");
    p["proxy"] = json!("");
    let site = call(&app, "POST", "/api/v1/sites", "management-secret", p).await;
    let site_id = site["id"].as_str().unwrap();

    let patch_resp = app.clone().oneshot(request("PATCH", &format!("/api/v1/sites/{site_id}"), Some("management-secret"), json!({
        "maintenance_via": "http",
        "proxy": "http://proxy.example:8080"
    }))).await.unwrap();
    assert_eq!(patch_resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn site_write_rejects_invalid_cdp_url() {
    let tmp = tempdir().unwrap();
    let app = app(&tmp);
    let mut p = payload();
    p["cdp_url"] = json!("wss://cdp-user:cdp-password@browser.example/cdp?token=x");
    let resp = app.clone().oneshot(request("POST", "/api/v1/sites", Some("management-secret"), p)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = crate::common::json_body(resp).await;
    assert_eq!(body["error"]["code"], "site.invalid_endpoint");
}
