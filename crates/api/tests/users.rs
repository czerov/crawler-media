use std::path::Path;
use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Media, MediaId, MediaKind, UserId};
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
    router(
        ApiState::new(
            Store::open(root.join("data")).unwrap(),
            "admin-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(root.join("stage"))),
            root.join("library"),
        )
        .unwrap(),
    )
}

fn request(method: &str, uri: &str, token: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn body(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn json_data(response: axum::response::Response) -> Value {
    let status = response.status();
    let b = body(response).await;
    assert_eq!(
        b["ok"], true,
        "expected v1 ok=true, status={status}, body={b}"
    );
    b.get("data").cloned().unwrap_or(Value::Null)
}

fn subscribe() -> Value {
    json!({
        "media": { "kind": "movie", "title": "The Matrix", "tmdb_id": "603" },
        "coverage": { "kind": "movie" },
        "fetch_mode": "search",
    })
}

#[tokio::test]
async fn two_users_sign_in_and_see_only_their_subscribes() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(tmp.path());
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/users",
            "admin-token",
            json!({ "login": "alice", "password": "alice-password" }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let alice = json_data(created).await;
    let alice_login = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            "",
            json!({ "username": "alice", "password": "alice-password" }),
        ))
        .await
        .unwrap();
    let alice_token = json_data(alice_login).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    app.clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            &alice_token,
            subscribe(),
        ))
        .await
        .unwrap();
    let alice_list = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/subscriptions",
            &alice_token,
            Value::Null,
        ))
        .await
        .unwrap();
    let admin_list = app
        .oneshot(request(
            "GET",
            "/api/v1/subscriptions",
            "admin-token",
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(json_data(alice_list).await.as_array().unwrap().len(), 1);
    // 管理员在 /api/v1 拥有全局视图，可以看到全部订阅
    assert_eq!(json_data(admin_list).await.as_array().unwrap().len(), 1);
    assert!(alice["id"].as_str().is_some());
}

#[tokio::test]
async fn list_users_returns_logins_without_tokens() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(tmp.path());
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/users",
            "admin-token",
            json!({ "login": "alice", "password": "alice-password" }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let listed = app
        .oneshot(request("GET", "/api/v1/users", "admin-token", Value::Null))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let rows = json_data(listed).await;
    let rows = rows.as_array().unwrap();
    assert!(rows.iter().any(|row| row["login"] == "alice"));
    assert!(rows.iter().any(|row| row["login"] == "admin"));
    for row in rows {
        assert!(row.get("token").is_none());
        assert!(row["id"].as_str().is_some());
    }
}

#[test]
fn playback_progress_is_isolated_per_user() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let alice = UserId::new();
    let bob = UserId::new();
    store.set_playback_progress(alice, media.id, 1200).unwrap();
    store.set_playback_progress(bob, media.id, 9000).unwrap();
    assert_eq!(
        store.playback_progress(alice, media.id).unwrap(),
        Some(1200)
    );
    assert_eq!(store.playback_progress(bob, media.id).unwrap(), Some(9000));
}

#[tokio::test]
async fn invalid_token_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let response = app(tmp.path())
        .oneshot(request(
            "GET",
            "/api/v1/subscriptions",
            "wrong",
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
