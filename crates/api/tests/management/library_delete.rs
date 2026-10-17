use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;

#[tokio::test]
async fn delete_library_item_removes_file_and_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let source = tmp.path().join("matrix.mkv");
    std::fs::write(
        &source,
        r#"{"resolution":"2160p","codec":"hevc","hdr":"hdr10"}"#,
    )
    .unwrap();
    downloader.map_enclosure("https://pt.example/download.php?id=1&passkey=abc", source);
    let app = router(state(tmp.path(), fetcher, downloader));
    create_site(&app).await;
    let subscribe = create_subscribe(&app, "search").await;
    let run = app
        .clone()
        .oneshot(request(
            "POST",
            &format!(
                "/api/v1/subscriptions/{}/run",
                subscribe["id"].as_str().unwrap()
            ),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(run.status(), StatusCode::OK);

    let libraries = json_body(
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
    let movie_lib = libraries["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == "movie")
        .expect("movie library");
    let library_id = movie_lib["id"].as_str().unwrap();
    let items = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{library_id}/items"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let item = &items["data"].as_array().unwrap()[0];
    let media_id = item["media_item_id"].as_str().unwrap();

    let ledger = json_data(
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
    let path = ledger[0]["path"].as_str().unwrap().to_string();
    assert!(Path::new(&path).is_file());

    let deleted = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/libraries/{library_id}/items/{media_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    let body = json_body(deleted).await;
    assert_eq!(body["data"]["rows_deleted"], 1);
    assert!(!Path::new(&path).exists(), "library file should be gone");

    let ledger_after = json_data(
        app.oneshot(request(
            "GET",
            "/api/v1/ledger",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert!(ledger_after.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn default_library_cannot_delete_or_retransfer_orphaned_outside_root() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader));
    let outside = tmp.path().join("outside/orphan.mkv");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, b"orphan").unwrap();
    let source = tmp.path().join("source.mkv");
    std::fs::write(&source, b"source").unwrap();

    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "Orphan".into(),
        year: Some(1999),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let row = domain::LedgerRow {
        id: domain::LedgerId::new(),
        media_id: media.id,
        path: outside.display().to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: domain::QualitySource::Release,
        confidence: domain::Confidence::High,
        filter_score: None,
    };
    store.insert_ledger_with_source(&row, &source).unwrap();
    let default_lib = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .expect("default movie library");
    drop(store);

    let deleted = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/libraries/{}/items/{}", default_lib.id, media.id),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NOT_FOUND);
    assert!(outside.exists(), "默认库绝不能删除库外文件");

    let blocked = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/ledger/{}?delete_file=true", row.id),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::FORBIDDEN);
    assert!(outside.exists(), "raw ledger 删除也不能动库外文件");

    std::fs::remove_file(&outside).unwrap();
    let retransfer = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/ledger/{}/retransfer", row.id),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(retransfer.status(), StatusCode::FORBIDDEN);
    assert!(!outside.exists(), "retransfer 绝不能把文件写回库外路径");
}
