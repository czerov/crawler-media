//! Organize: preview computes template targets, apply renames files and
//! updates the ledger; gallery groups images; single-item refresh 200s.

use std::collections::HashMap;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use domain::MediaKind;
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

async fn seed_movie_lib(app: &axum::Router, tmp: &tempfile::TempDir) -> (String, String) {
    let movie = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&movie).unwrap();
    // 文件名不合命名模板：标题带点号、无年份括号。
    std::fs::write(movie.join("Matrix.1999.mkv"), b"matrix").unwrap();
    std::fs::write(movie.join("poster.jpg"), b"poster").unwrap();
    let libs = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/libraries",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let movie_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    scan_and_wait(&app, &movie_id).await;
    let ledger = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/ledger",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ledger["data"][0]["library_id"], movie_id);
    let media_id = ledger["data"][0]["media_id"].as_str().unwrap().to_string();
    (movie_id, media_id)
}

#[tokio::test]
async fn organize_preview_and_apply_renames_files() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (movie_id, _media_id) = seed_movie_lib(&app, &tmp).await;
    let movie = tmp.path().join("data/library/movies");
    let original = movie.join("Matrix.1999.mkv");
    assert!(original.is_file());

    let preview = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{movie_id}/organize-preview"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        preview["data"]["renames"].as_array().unwrap().len() >= 1,
        "{preview}"
    );
    let from = preview["data"]["renames"][0]["from"].as_str().unwrap();
    let to = preview["data"]["renames"][0]["to"].as_str().unwrap();
    assert_eq!(from, original.display().to_string());
    assert!(to.contains("Matrix (1999)"), "目标名按命名模板生成: {to}");

    let applied = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{movie_id}/organize"),
            Some("management-secret"),
            json!({ "renames": [{ "from": from, "to": to }] }),
        ))
        .await
        .unwrap();
    assert_eq!(applied.status(), StatusCode::OK);
    let body = json_body(applied).await;
    assert_eq!(body["data"]["applied"], 1, "{body}");
    assert!(std::path::Path::new(to).is_file(), "file renamed: {to}");
    assert!(!original.exists(), "old name gone");

    let ledger = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/ledger",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        ledger["data"][0]["path"], to,
        "ledger path follows the rename"
    );
}

#[tokio::test]
async fn gallery_and_item_refresh_respond() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (movie_id, media_id) = seed_movie_lib(&app, &tmp).await;

    let gallery = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{movie_id}/gallery"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let groups = gallery["data"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "gallery has the owned item group");
    assert_eq!(groups[0]["title"], "Matrix");

    let refresh = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{movie_id}/items/{media_id}/metadata/refresh"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(refresh.status(), StatusCode::OK);
}

#[tokio::test]
async fn item_refresh_rejects_media_owned_by_another_library() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (_movie_id, media_id) = seed_movie_lib(&app, &tmp).await;
    let other_root = tmp.path().join("other-movies");
    std::fs::create_dir_all(&other_root).unwrap();
    let other = Store::open(tmp.path().join("data"))
        .unwrap()
        .create_library(
            MediaKind::Movie,
            "Other",
            &[other_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let refresh = app
        .oneshot(request(
            "POST",
            &format!(
                "/api/v1/libraries/{}/items/{media_id}/metadata/refresh",
                other.id
            ),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(refresh.status(), StatusCode::NOT_FOUND);
}
