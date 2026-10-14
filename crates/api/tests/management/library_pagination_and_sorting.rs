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
async fn library_manage_scope_shows_admin_hidden_libraries() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    // 1. 获取默认 movie 库 ID 并将其 admin_visible 改为 false
    let list_res = json_data(
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
    let movie_lib = list_res
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap();
    let movie_id = movie_lib["id"].as_str().unwrap();

    let patch_res = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/libraries/{movie_id}"),
            Some("management-secret"),
            json!({ "admin_visible": false }),
        ))
        .await
        .unwrap();
    assert_eq!(patch_res.status(), StatusCode::OK);

    // 2. 普通浏览列表（无 manage=1）：该库已被隐藏
    let browse_res = json_data(
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
    assert!(
        !browse_res
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["id"] == movie_id),
        "普通浏览列表不应展示 admin_visible: false 的库"
    );

    // 3. 管理列表（带 manage=1）：管理员依然能看到全部媒体库供管理恢复
    let manage_res = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/libraries?manage=1",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        manage_res
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["id"] == movie_id),
        "管理列表 (manage=1) 必须展示全部库供管理员维护"
    );
}

#[tokio::test]
async fn library_items_respect_limit_and_offset() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let movie_dir = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&movie_dir).unwrap();
    std::fs::write(movie_dir.join("Alpha.1999.1080p.mkv"), b"1").unwrap();
    std::fs::write(movie_dir.join("Beta.2000.1080p.mkv"), b"2").unwrap();
    std::fs::write(movie_dir.join("Gamma.2001.1080p.mkv"), b"3").unwrap();

    let libs = json_data(
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
    let movie_id = libs
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // 扫描库
    scan_and_wait(&app, &movie_id).await;

    // 全量是 3 个
    let all = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{movie_id}/items?sort=title&order=asc"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(all.as_array().unwrap().len(), 3);

    // limit=1, offset=1 应只返回 1 个（且是第二个元素 Beta）
    let paged = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                &format!(
                    "/api/v1/libraries/{movie_id}/items?sort=title&order=asc&limit=1&offset=1"
                ),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let paged_items = paged.as_array().unwrap();
    assert_eq!(paged_items.len(), 1, "limit=1 应该只返回 1 个元素");
    assert_eq!(paged_items[0]["title"], "Beta");
}
