use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use domain::{Confidence, LedgerId, LedgerRow, QualitySource, SubscribeId};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use subscribe::QualityFact;
use tower::ServiceExt;

use super::common::*;

#[tokio::test]
async fn wash_cut_deletes_previous_library_file_and_ledger_row() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let better = tmp.path().join("better.mkv");
    // S1: replacement needs a successful probe of the new file.
    install_fake_ffprobe();
    std::fs::write(&better, probed_video(2160, "hevc", true)).unwrap();
    downloader.map_enclosure("https://pt.example/download.php?id=1&passkey=abc", better);

    let app = router(state(tmp.path(), fetcher, downloader));
    create_site(&app).await;
    let mut body = subscribe_payload("search");
    body["wash_cut"] = json!(true);
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_data(created).await;
    let subscribe_id = created["id"].as_str().unwrap();
    let media_id = created["media"]["id"].as_str().unwrap();

    let old = tmp.path().join("data/library/movies/old.mkv");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, b"old-1080p").unwrap();
    {
        let store = Store::open(tmp.path().join("data")).unwrap();
        store
            .insert_ledger(&LedgerRow {
                id: LedgerId::new(),
                media_id: media_id.parse().unwrap(),
                path: old.display().to_string(),
                season: None,
                episode: None,
                resolution: Some("1080p".into()),
                codec: Some("h264".into()),
                hdr: None,
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                filter_score: Some(50),
            })
            .unwrap();
        let mut facts = subscribe::SubscribeFacts::default();
        facts.upsert(
            None,
            None,
            QualityFact {
                score: 50,
                path: Some(old.display().to_string()),
            },
        );
        store
            .save_subscribe_facts(SubscribeId::from_str(subscribe_id).unwrap(), &facts)
            .unwrap();
    }

    let run = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{subscribe_id}/run"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(run.status(), StatusCode::OK);

    assert!(!old.exists(), "previous Library file must be deleted");
    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/ledger",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let rows = json_data(listed).await;
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0]["path"].as_str().unwrap(), old.display().to_string());
    assert!(Path::new(rows[0]["path"].as_str().unwrap()).is_file());
}

#[tokio::test]
async fn wash_cut_drops_ledger_when_previous_file_is_already_gone() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let better = tmp.path().join("better.mkv");
    // S1: replacement needs a successful probe of the new file.
    install_fake_ffprobe();
    std::fs::write(&better, probed_video(2160, "hevc", true)).unwrap();
    downloader.map_enclosure("https://pt.example/download.php?id=1&passkey=abc", better);

    let app = router(state(tmp.path(), fetcher, downloader));
    create_site(&app).await;
    let mut body = subscribe_payload("search");
    body["wash_cut"] = json!(true);
    let created = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                body,
            ))
            .await
            .unwrap(),
    )
    .await;
    let subscribe_id = created["id"].as_str().unwrap();
    let media_id = created["media"]["id"].as_str().unwrap();
    let gone = tmp.path().join("data/library/movies/missing.mkv");
    {
        let store = Store::open(tmp.path().join("data")).unwrap();
        store
            .insert_ledger(&LedgerRow {
                id: LedgerId::new(),
                media_id: media_id.parse().unwrap(),
                path: gone.display().to_string(),
                season: None,
                episode: None,
                resolution: Some("1080p".into()),
                codec: Some("h264".into()),
                hdr: None,
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                filter_score: Some(50),
            })
            .unwrap();
        let mut facts = subscribe::SubscribeFacts::default();
        facts.upsert(
            None,
            None,
            QualityFact {
                score: 50,
                path: Some(gone.display().to_string()),
            },
        );
        store
            .save_subscribe_facts(SubscribeId::from_str(subscribe_id).unwrap(), &facts)
            .unwrap();
    }

    let run = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{subscribe_id}/run"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(run.status(), StatusCode::OK);
    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/ledger",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let rows = json_data(listed).await;
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_ne!(
        rows[0]["path"].as_str().unwrap(),
        gone.display().to_string()
    );
}
