//! Season details / similar / release forecast endpoints — shapes under an
//! empty catalog (no TMDB key) and empty history.

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

async fn seed_tv(tmp: &tempfile::TempDir, app: &axum::Router) -> (String, String) {
    let tv = tmp.path().join("data/library/tv");
    std::fs::create_dir_all(&tv).unwrap();
    std::fs::write(tv.join("The.Expanse.S01E01.1080p.mkv"), b"e").unwrap();
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
    let tv_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "tv")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    scan_and_wait(&app, &tv_id).await;
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
    (
        tv_id,
        ledger["data"][0]["media_id"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn episodes_fallback_to_filename_without_catalog() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (tv_id, media_id) = seed_tv(&tmp, &app).await;
    let episodes = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{tv_id}/items/{media_id}/episodes?season=1"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let list = episodes["data"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["episode_number"], 1);
    // 无 TMDB 时退回文件名；overview 为 null（不报错）。
    assert!(list[0]["name"].as_str().unwrap().contains(".mkv"));
    assert!(list[0]["overview"].is_null());
}

#[tokio::test]
async fn episodes_are_sorted_by_episode_number_ascending() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let tv = tmp.path().join("data/library/tv/Chernobyl.2019.S01");
    std::fs::create_dir_all(&tv).unwrap();
    // 故意乱序写入分集文件：先写第 3 集、再写第 1 集、最后写第 2 集
    std::fs::write(tv.join("Chernobyl.S01E03.mkv"), b"ep3").unwrap();
    std::fs::write(tv.join("Chernobyl.S01E01.mkv"), b"ep1").unwrap();
    std::fs::write(tv.join("Chernobyl.S01E02.mkv"), b"ep2").unwrap();

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
    let tv_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "tv")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    scan_and_wait(&app, &tv_id).await;

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
    let media_id = ledger["data"][0]["media_id"].as_str().unwrap().to_string();

    let episodes = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{tv_id}/items/{media_id}/episodes?season=1"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let list = episodes["data"].as_array().unwrap();
    let numbers: Vec<i64> = list
        .iter()
        .map(|e| e["episode_number"].as_i64().unwrap())
        .collect();
    assert_eq!(
        numbers,
        vec![1, 2, 3],
        "无论文件物理写入顺序如何，分集严格按 1, 2, 3 顺序展示"
    );
}

#[tokio::test]
async fn similar_and_gallery_and_forecast_respond() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (tv_id, media_id) = seed_tv(&tmp, &app).await;

    let similar = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{tv_id}/items/{media_id}/similar"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        similar["data"].as_array().unwrap().is_empty(),
        "no catalog → empty"
    );

    let gallery_resp = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/playback/favorites/gallery",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let gallery = json_body(gallery_resp).await;
    assert!(
        gallery["data"].as_array().unwrap().is_empty(),
        "no favorites yet"
    );

    // 无历史：forecast 返回空 days。
    let subscribe = create_subscribe(&app, "search").await;
    eprintln!("SUBSCRIBE={}", subscribe);
    let forecast = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!(
                    "/api/v1/subscriptions/{}/release-forecast",
                    subscribe["id"].as_str().unwrap()
                ),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(forecast["data"]["samples"], 0);
    assert!(forecast["data"]["days"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn languages_endpoint_returns_list_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let body = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/settings/languages",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    // 无 TMDB key：返回空表但不报错（设置页搜索候选为空时回退预设）。
    assert!(body["data"]["languages"].as_array().is_some(), "{body}");
}

/// 同一集的两个并行版本只算一集：`episode_count` 是集数，不是台账行数。
#[tokio::test]
async fn wall_episode_count_ignores_parallel_versions_of_the_same_episode() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let tv = tmp.path().join("data/library/tv");
    std::fs::create_dir_all(&tv).unwrap();
    std::fs::write(tv.join("The.Expanse.S01E01.2160p.mkv"), b"e2").unwrap();
    let (tv_id, media_id) = seed_tv(&tmp, &app).await;
    let items = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{tv_id}/items"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let row = items["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["media_item_id"] == media_id.as_str())
        .unwrap();
    assert_eq!(
        row["file_count"], 2,
        "both parallel files stay in the library"
    );
    assert_eq!(
        row["episode_count"], 1,
        "two versions of S01E01 are one episode"
    );
    assert_eq!(
        row["seasons"],
        json!([1]),
        "duplicate season numbers are deduplicated"
    );
}
