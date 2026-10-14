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
async fn upload_artwork_larger_than_2mb_succeeds_without_axum_body_limit_rejection() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let movie_dir = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&movie_dir).unwrap();
    std::fs::write(movie_dir.join("BigArt.1999.1080p.mkv"), b"video").unwrap();

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
    let movie_lib = libs
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap();
    let movie_id = movie_lib["id"].as_str().unwrap();

    scan_and_wait(&app, &movie_id).await;

    let items = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{movie_id}/items"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let item_id = items.as_array().unwrap()[0]["media_item_id"]
        .as_str()
        .unwrap();

    // 生成约 2.5MB 的有效 base64 字符串（超过 Axum 默认的 2MB 限制）
    use base64::Engine;
    let raw_bytes = vec![0u8; 2_500_000];
    let encoded = base64::engine::general_purpose::STANDARD.encode(&raw_bytes);
    let data_url = format!("data:image/jpeg;base64,{encoded}");

    let upload_res = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{movie_id}/items/{item_id}/artwork/upload"),
            Some("management-secret"),
            json!({ "data_url": data_url }),
        ))
        .await
        .unwrap();

    assert_eq!(
        upload_res.status(),
        StatusCode::OK,
        "2.5MB base64 图片上传不应被 413 拦截"
    );
}

#[tokio::test]
async fn extras_only_detaches_specified_file_ids_not_whole_media() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let tv_dir = tmp.path().join("data/library/tv/Show/Season 01");
    std::fs::create_dir_all(&tv_dir).unwrap();
    std::fs::write(tv_dir.join("Show.S01E01.mkv"), b"ep1").unwrap();
    std::fs::write(tv_dir.join("Show.S01E02.mkv"), b"ep2").unwrap();

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
    let tv_lib = libs
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "tv")
        .unwrap();
    let tv_id = tv_lib["id"].as_str().unwrap();

    scan_and_wait(&app, tv_id).await;

    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let ledger = store.list_ledger().unwrap();
    assert_eq!(ledger.len(), 2);
    let ep1_id = ledger[0].id.to_string();
    let ep2_id = ledger[1].id.to_string();

    // 仅把 ep1 标为 extras
    let extras_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/reidentify/extras",
            Some("management-secret"),
            json!({ "file_ids": [ep1_id] }),
        ))
        .await
        .unwrap();
    assert_eq!(extras_res.status(), StatusCode::OK);

    // 验证：ep1 从 ledger 移出，ep2 仍安然保存在 ledger 中
    let remaining = store.list_ledger().unwrap();
    assert_eq!(remaining.len(), 1, "只应移出 1 个指定的 file_id");
    assert_eq!(remaining[0].id.to_string(), ep2_id);
}
