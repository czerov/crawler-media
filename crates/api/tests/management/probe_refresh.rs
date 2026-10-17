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

#[tokio::test]
async fn manual_probe_refreshes_every_library_holding_the_media() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let store = Store::open(&data).unwrap();
    let primary = data.join("library/movies");
    let secondary = data.join("other-movies");
    std::fs::create_dir_all(&primary).unwrap();
    std::fs::create_dir_all(&secondary).unwrap();
    let primary_id = store.default_library(MediaKind::Movie).unwrap().unwrap().id;
    store
        .create_library(
            MediaKind::Movie,
            "other",
            &[secondary.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Shared Media".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let mut ids = Vec::new();
    for root in [&primary, &secondary] {
        let path = root.join("Shared.Media.2024.mkv");
        std::fs::write(&path, b"invalid-video").unwrap();
        let row = LedgerRow {
            id: LedgerId::new(),
            media_id: media.id,
            path: path.display().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        };
        store.insert_ledger(&row).unwrap();
        store
            .put_cached_chapters(
                &row.id.to_string(),
                &[library::ChapterMarker {
                    start_ms: 12_000,
                    end_ms: 30_000,
                    title: Some("Existing chapter".into()),
                    marker_type: None,
                    synthetic: false,
                }],
            )
            .unwrap();
        ids.push(row.id.to_string());
    }
    drop(store);
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader));
    let response = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{primary_id}/items/{}/probe", media.id),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"]["queued"], 2);
    let store = Store::open(&data).unwrap();
    for id in ids {
        assert_eq!(
            store.get_cached_chapters(&id).unwrap().unwrap()[0]
                .title
                .as_deref(),
            Some("Existing chapter"),
            "the previous chapter result must remain visible while refresh runs: {id}"
        );
    }
    let job = store
        .latest_probe_job_for_scope(&format!("manual-probe:{}", media.id))
        .unwrap()
        .unwrap();
    assert_eq!(
        job.total, 2,
        "all files must be reserved in one persisted job"
    );
}

#[tokio::test]
async fn playback_session_returns_cached_chapters_without_reprobing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let store = Store::open(&data).unwrap();
    let root = data.join("library/movies");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("Cached.2024.mkv");
    std::fs::write(&path, b"not-a-video").unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Cached".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: path.display().to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).unwrap();
    store
        .put_cached_chapters(
            &row.id.to_string(),
            &[library::ChapterMarker {
                start_ms: 12_000,
                end_ms: 20_000,
                title: Some("Opening".into()),
                marker_type: None,
                synthetic: false,
            }],
        )
        .unwrap();
    drop(store);
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader));
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/sessions",
            Some("management-secret"),
            json!({ "media_item_id": media.id.to_string() }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"]["chapters"][0]["start_ms"], 12_000);
    assert_eq!(body["data"]["chapters_pending"], false);
    assert_eq!(body["data"]["file_id"], row.id.to_string());

    let cache_url = format!("/api/v1/playback/chapters?file_id={}", row.id);
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            &cache_url,
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"]["ready"], true);
    assert_eq!(body["data"]["chapters"][0]["start_ms"], 12_000);

    Store::open(&data)
        .unwrap()
        .clear_cached_chapters(&row.id.to_string())
        .unwrap();
    let response = app
        .oneshot(request(
            "GET",
            &cache_url,
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["data"]["ready"], false);
}
