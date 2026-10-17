//! Tests for Jellyfin chapters and intro/outro markers support.

use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
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
async fn jellyfin_playback_info_and_items_include_chapters_with_markers() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, store) = app(&tmp);

    let tv_dir = tmp.path().join("data/library/tv/Frieren");
    std::fs::create_dir_all(&tv_dir).unwrap();

    let strm_file = tv_dir.join("Frieren.S01E01.strm");
    std::fs::write(&strm_file, "https://cdn.example.com/frieren/ep01.mkv\n").unwrap();

    // 1. Manually add Media and LedgerRow
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Tv,
        title: "葬送的芙莉莲".into(),
        year: Some(2023),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("12345".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let media = store.ensure_media(media).unwrap();

    let row = domain::LedgerRow {
        id: domain::LedgerId::new(),
        media_id: media.id,
        path: strm_file.display().to_string(),
        season: Some(1),
        episode: Some(1),
        resolution: Some("1080p".into()),
        codec: Some("hevc".into()),
        hdr: None,
        quality_source: domain::QualitySource::Release,
        confidence: domain::Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).unwrap();

    // 2. Add an intro marker in media_markers table (simulate TheIntroDB or previous scan)
    let marker = api::store::StoredMediaMarker {
        media_id: media.id,
        season: 1,
        episode: 1,
        intro_start_ms: Some(95_000),
        intro_end_ms: Some(185_000),
        outro_start_ms: Some(1_320_000),
        outro_end_ms: Some(1_410_000),
        source: "theintrodb".into(),
        locked: false,
        updated_at: 0,
    };
    store.put_media_marker(&marker).unwrap();

    // 3. Query Jellyfin /Items (protected route merged at root)
    let res = app
        .clone()
        .oneshot(request(
            "GET",
            "/Items",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = json_body(res).await;
    let items = body["Items"].as_array().unwrap();
    assert_eq!(items.len(), 1);

    let item = &items[0];
    let chapters = item["Chapters"]
        .as_array()
        .expect("Chapters should be present");
    assert!(!chapters.is_empty(), "Chapters should contain markers");

    let intro = chapters
        .iter()
        .find(|c| c["MarkerType"] == "IntroStart")
        .expect("IntroStart marker");
    assert_eq!(intro["StartPositionTicks"], 950_000_000i64); // 95s * 10,000,000

    let credits = chapters
        .iter()
        .find(|c| c["MarkerType"] == "CreditsStart")
        .expect("CreditsStart marker");
    assert_eq!(credits["StartPositionTicks"], 13_200_000_000i64); // 1320s * 10,000,000

    // 4. Query Jellyfin /Items/{id}/PlaybackInfo
    let playback_res = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/Items/{}/PlaybackInfo", item["Id"].as_str().unwrap()),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(playback_res.status(), StatusCode::OK);
    let pb_body = json_body(playback_res).await;
    let pb_chapters = pb_body["Chapters"]
        .as_array()
        .expect("Chapters in PlaybackInfo");
    assert_eq!(pb_chapters.len(), chapters.len());
}
