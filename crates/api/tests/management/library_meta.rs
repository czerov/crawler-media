//! Item-detail metadata: NFO sidecars feed overview/rating/runtime/cast and
//! fanart.jpg backs backdrop_url.

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
async fn item_detail_reads_nfo_metadata_and_fanart() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let movie = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&movie).unwrap();
    let video = movie.join("The.Matrix.1999.2160p.mkv");
    std::fs::write(&video, b"matrix").unwrap();

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
    // 扫描会写自己的最小 NFO，测试的富 NFO 在扫描后落盘（模拟外部刮削器产物）。
    std::fs::write(
        movie.join("The.Matrix.1999.2160p.nfo"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<movie>
  <title>The Matrix</title>
  <year>1999</year>
  <plot>A hacker discovers the truth about reality.</plot>
  <rating>8.7</rating>
  <runtime>136</runtime>
  <genre>Action</genre>
  <actor><name>Keanu Reeves</name><role>Neo</role></actor>
</movie>
"#,
    )
    .unwrap();
    std::fs::write(movie.join("fanart.jpg"), b"\xff\xd8\xff\xdb").unwrap();

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
    let detail = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{movie_id}/items/{media_id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        detail["data"]["overview"],
        "A hacker discovers the truth about reality."
    );
    assert_eq!(detail["data"]["rating"], "8.7");
    assert_eq!(detail["data"]["runtime_minutes"], "136");
    assert_eq!(detail["data"]["genres"][0], "Action");
    assert_eq!(detail["data"]["cast"][0]["name"], "Keanu Reeves");
    let backdrop = detail["data"]["backdrop_url"].as_str().unwrap().to_string();
    assert!(backdrop.starts_with("/fanart/"), "{backdrop}");

    let fanart = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1{backdrop}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(fanart.status(), StatusCode::OK);
}

#[tokio::test]
async fn episode_still_url_and_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let tv = tmp.path().join("data/library/tv");
    std::fs::create_dir_all(&tv).unwrap();
    let video = tv.join("The.Expanse.S01E01.1080p.mkv");
    std::fs::write(&video, b"expanse").unwrap();
    std::fs::write(tv.join("still.jpg"), b"\xff\xd8\xff\xdb").unwrap();

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
                &format!("/api/v1/libraries/{tv_id}/items/{media_id}/episodes"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let still = episodes["data"][0]["still_url"].as_str().unwrap();
    assert!(still.starts_with("/stills/"), "{still}");
    let bytes = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1{still}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(bytes.status(), StatusCode::OK);
}

#[tokio::test]
async fn sibling_episodes_serve_their_own_stills() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let tv = tmp.path().join("data/library/tv");
    std::fs::create_dir_all(&tv).unwrap();
    for (episode, bytes) in [
        (1, b"episode-one".as_slice()),
        (2, b"episode-two".as_slice()),
    ] {
        let video = tv.join(format!("The.Expanse.S01E{episode:02}.mkv"));
        std::fs::write(&video, b"video").unwrap();
        std::fs::write(
            video.with_file_name(format!("The.Expanse.S01E{episode:02}-still.jpg")),
            bytes,
        )
        .unwrap();
    }
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
    let id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "tv")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    scan_and_wait(&app, id).await;
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
    for row in ledger["data"].as_array().unwrap() {
        let episode = row["episode"].as_u64().unwrap();
        let compact = row["id"].as_str().unwrap().replace('-', "");
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/stills/{compact}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            bytes.as_ref(),
            format!(
                "episode-{word}",
                word = if episode == 1 { "one" } else { "two" }
            )
            .as_bytes()
        );
    }
}

#[tokio::test]
async fn episode_without_artwork_does_not_advertise_missing_fanart() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let tv = tmp.path().join("data/library/tv");
    std::fs::create_dir_all(&tv).unwrap();
    std::fs::write(tv.join("Spirit.Rangers.S01E01.1080p.strm"), b"https://cdn.example/ep.mkv")
        .unwrap();
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
    let id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "tv")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    scan_and_wait(&app, id).await;
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
    let episodes = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{id}/items/{media_id}/episodes"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(episodes["data"][0]["still_url"].is_null());
}
