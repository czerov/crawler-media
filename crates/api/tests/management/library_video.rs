//! "Other" (video) library kind: create, scan without identification, browse.

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

#[tokio::test]
async fn video_library_scan_browses_by_filename() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let root = tmp.path().join("homevideos");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("宝宝满月纪念.mp4"), b"video").unwrap();

    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/libraries",
            Some("management-secret"),
            json!({
                "name": "家庭录像",
                "kind": "video",
                "root_paths": [root.display().to_string()],
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let body = json_body(created).await;
    let id = body["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(body["data"]["kind"], "video");

    scan_and_wait(&app, &id).await;

    let items = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{id}/items"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let list = items["data"].as_array().unwrap();
    assert_eq!(list.len(), 1, "each video file becomes one item");
    assert_eq!(list[0]["title"], "宝宝满月纪念");
    assert_eq!(list[0]["kind"], "video");

    let detail = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!(
                    "/api/v1/libraries/{id}/items/{}",
                    list[0]["media_item_id"].as_str().unwrap()
                ),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        detail["data"]["files"][0]["path"],
        root.join("宝宝满月纪念.mp4").display().to_string()
    );
}
