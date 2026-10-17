//! 合集 / 重新识别 / 路径核对 / 根目录归并 / countries / hardware。

use std::collections::HashMap;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
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
async fn collections_crud_and_items() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    // 建一个 media + ledger 供加入。
    let store = Store::open(tmp.path().join("data")).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
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
    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: media.id,
            path: tmp
                .path()
                .join("data/library/movies/Matrix.mkv")
                .display()
                .to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();
    // store 层直测：collections 表是否已迁移。
    let probe = Store::open(tmp.path().join("data")).unwrap();
    let probe_user = domain::UserId::new();
    let probe_id = probe.create_collection(probe_user, "直测").unwrap();
    let probe_list = probe.list_collections(probe_user).unwrap();
    assert_eq!(probe_list.len(), 1, "store 层集合可用");
    let _ = probe.delete_collection(&probe_id);
    drop(probe);
    drop(store);

    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/collections",
                Some("management-secret"),
                json!({ "name": "周末片单" }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap().to_string();

    let added = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/collections/{id}/items"),
            Some("management-secret"),
            json!({ "media_item_id": media.id.to_string() }),
        ))
        .await
        .unwrap();
    assert_eq!(added.status(), StatusCode::OK);

    let items = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/collections/{id}/items"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(items["data"].as_array().unwrap().len(), 1);
    assert_eq!(items["data"][0]["title"], "The Matrix");

    let list = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/collections",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(list["data"].as_array().unwrap().len(), 1);
    assert_eq!(list["data"][0]["item_count"], 1);

    let removed = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/collections/{id}/items/{}", media.id),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::OK);

    let deleted = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/collections/{id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
}

#[tokio::test]
async fn path_reconciliation_and_root_consolidation() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let movie = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&movie).unwrap();
    let moved = movie.join("Moved").join("Matrix.1999.mkv");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::write(&moved, b"m").unwrap();
    // 台账指向原位（不存在），库内候选在 Moved/ 下。
    let store = Store::open(tmp.path().join("data")).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
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
    let original = movie.join("Matrix.1999.mkv");
    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: media.id,
            path: original.display().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();
    drop(store);

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

    let preview = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{movie_id}/path-reconciliation-preview"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let missing = preview["data"]["missing"].as_array().unwrap();
    assert_eq!(missing.len(), 1, "台账失联行被报出");
    assert_eq!(missing[0]["file_name"], "Matrix.1999.mkv");
    assert_eq!(
        missing[0]["candidates"].as_array().unwrap()[0],
        moved.display().to_string()
    );

    let reconciled = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{movie_id}/path-reconciliations"),
            Some("management-secret"),
            json!({ "reconciliations": [{ "from": original.display().to_string(), "to": moved.display().to_string() }] }),
        ))
        .await
        .unwrap();
    assert_eq!(reconciled.status(), StatusCode::OK);
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
    assert_eq!(ledger["data"][0]["path"], moved.display().to_string());

    // 根目录归并：给库加一个指向同一物理目录的根。
    let dup_root = movie.join("same").join("..");
    std::fs::create_dir_all(&dup_root).unwrap();
    let patch = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/libraries/{movie_id}"),
            Some("management-secret"),
            json!({ "root_paths": [movie.display().to_string(), dup_root.display().to_string()] }),
        ))
        .await
        .unwrap();
    assert_eq!(patch.status(), StatusCode::OK);
    let dup = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{movie_id}/root-consolidation-preview"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(dup["data"]["duplicates"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn countries_hardware_and_reidentify_shapes() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let countries = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/settings/countries",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(countries["data"]["countries"].as_array().is_some());

    let hardware = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/hardware",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(hardware["data"]["hw_backend"].is_null());

    // 建 movie 文件 + 扫描 → preview reidentify 形状。
    let movie = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&movie).unwrap();
    std::fs::write(movie.join("Dune.2021.mkv"), b"d").unwrap();
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
    let media_id = ledger["data"][0]["media_id"].as_str().unwrap();
    let preview = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{movie_id}/items/{media_id}/reidentification-preview"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(preview["data"]["groups"].as_array().unwrap().len(), 1);
    assert!(
        preview["data"]["search_seed"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("dune")
    );
}
