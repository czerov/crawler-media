use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;

fn app(tmp: &tempfile::TempDir) -> (axum::Router, api::Store) {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let st = state(tmp.path(), fetcher, downloader);
    (router(st), store)
}

#[tokio::test]
async fn concurrent_manual_refresh_returns_same_active_job() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, store) = app(&tmp);

    let tv_dir = tmp.path().join("data/library/tv/TestTV");
    std::fs::create_dir_all(&tv_dir).unwrap();

    let strm_file = tv_dir.join("TestTV.S01E01.strm");
    std::fs::write(&strm_file, "https://cdn.example.com/test/ep01.mkv\n").unwrap();

    let media_id = MediaId::new();
    let media = Media {
        id: media_id,
        kind: MediaKind::Tv,
        title: "Test TV Refresh".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let media = store.ensure_media(media).unwrap();

    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: strm_file.display().to_string(),
        season: Some(1),
        episode: Some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).unwrap();

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
    let tv_lib_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "tv")
        .unwrap()["id"]
        .as_str()
        .unwrap();

    let refresh_url = format!(
        "/api/v1/libraries/{tv_lib_id}/items/{}/chapters/refresh?season=1&episode=1",
        media.id
    );

    let res1 = app
        .clone()
        .oneshot(request(
            "POST",
            &refresh_url,
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(res1.status(), StatusCode::OK);
    let data1 = json_body(res1).await;

    let res2 = app
        .clone()
        .oneshot(request(
            "POST",
            &refresh_url,
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(res2.status(), StatusCode::OK);
    let data2 = json_body(res2).await;

    assert_eq!(
        data1["data"]["fingerprint_refresh_job"]["id"],
        data2["data"]["fingerprint_refresh_job"]["id"]
    );
    assert_eq!(data2["data"]["fingerprint_refresh_already_running"], true);

    let status_url = format!(
        "/api/v1/libraries/{tv_lib_id}/items/{}/probe-status?season=1",
        media.id
    );
    let status_res = app
        .clone()
        .oneshot(request(
            "GET",
            &status_url,
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(status_res.status(), StatusCode::OK);
    let status_data = json_body(status_res).await;
    assert!(status_data["data"]["job"]["metrics"]["input_bytes"].is_null());
}
