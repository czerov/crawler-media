use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    router(state(tmp.path(), fetcher, downloader))
}

async fn get(app: &axum::Router, uri: &str) -> Value {
    let response = app
        .clone()
        .oneshot(request("GET", uri, Some("management-secret"), Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

#[tokio::test]
async fn libraries_seed_default_movie_and_tv() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let listed = get(&app, "/api/v1/libraries").await;
    let rows = listed["data"].as_array().expect("array of libraries");
    assert_eq!(rows.len(), 2);

    let movie = rows.iter().find(|l| l["kind"] == "movie").unwrap();
    assert_eq!(movie["name"], "电影库");
    assert_eq!(movie["is_default"], true);

    let tv = rows.iter().find(|l| l["kind"] == "tv").unwrap();
    assert_eq!(tv["name"], "剧集库");
    assert_eq!(tv["is_default"], true);
}

#[tokio::test]
async fn libraries_create_with_two_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "华语电影",
                    "kind": "movie",
                    "root_paths": ["/data/movies/cn1", "/data/movies/cn2"],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(created["data"]["name"], "华语电影");
    assert_eq!(created["data"]["kind"], "movie");
    assert_eq!(created["data"]["is_default"], false);
    let roots = created["data"]["root_paths"].as_array().unwrap();
    assert_eq!(roots.len(), 2);
    assert_eq!(roots[0], "/data/movies/cn1");
    assert_eq!(roots[1], "/data/movies/cn2");
}

#[tokio::test]
async fn libraries_reject_unknown_kind_and_empty_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/libraries",
            Some("management-secret"),
            json!({
                "name": "非法",
                "kind": "music",
                "root_paths": ["/data/music"],
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/libraries",
            Some("management-secret"),
            json!({
                "name": "无根路径",
                "kind": "movie",
                "root_paths": [],
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn libraries_patch_replaces_roots_and_renames() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "欧美剧",
                    "kind": "tv",
                    "root_paths": ["/data/tv/us"],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap();

    let patched = json_body(
        app.clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/v1/libraries/{id}"),
                Some("management-secret"),
                json!({
                    "name": "欧美精选剧集",
                    "root_paths": ["/data/tv/us_archive", "/data/tv/us_current"],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(patched["data"]["name"], "欧美精选剧集");
    let roots = patched["data"]["root_paths"].as_array().unwrap();
    assert_eq!(roots.len(), 2);
    assert_eq!(roots[0], "/data/tv/us_archive");
    assert_eq!(roots[1], "/data/tv/us_current");
}

#[tokio::test]
async fn libraries_delete_extra_but_protect_last_of_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "临时电影库",
                    "kind": "movie",
                    "root_paths": ["/data/temp_movies"],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let temp_id = created["data"]["id"].as_str().unwrap();

    let deleted = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/libraries/{temp_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::ACCEPTED);

    let listed = get(&app, "/api/v1/libraries").await;
    let default_movie = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap();
    let default_id = default_movie["id"].as_str().unwrap();

    let rejected = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/libraries/{default_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::CONFLICT);
}
