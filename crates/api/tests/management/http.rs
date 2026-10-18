use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

#[tokio::test]
async fn subscribe_with_same_tmdb_id_reuses_media_row() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let mut first_body = subscribe_payload("search");
    first_body["media"]["tmdb_id"] = json!("603");
    let first = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            first_body,
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    let first = json_data(first).await;
    let mut second_body = subscribe_payload("rss");
    second_body["media"]["tmdb_id"] = json!("603");
    let second = app
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            second_body,
        ))
        .await
        .unwrap();
    // G-M1: the same media in the same library cannot be subscribed twice
    // with overlapping coverage; the second request resolves to the same
    // media row and is refused instead of duplicating it.
    assert_eq!(second.status(), StatusCode::CONFLICT);
    assert!(first["media"]["id"].is_string());
}

#[tokio::test]
async fn unauthenticated_management_request_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let app = router(state(
        tmp.path(),
        fetcher,
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));

    let response = app
        .oneshot(request("GET", "/api/v1/ledger", None, Value::Null))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn authenticated_api_creates_site_and_subscribe_and_lists_search_torrents() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let app = router(state(
        tmp.path(),
        fetcher,
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));

    let site = create_site(&app).await;
    assert_eq!(site["name"], "demo");
    assert!(site["id"].as_str().is_some());
    let subscribe = create_subscribe(&app, "search").await;
    assert_eq!(subscribe["fetch_mode"], "search");
    assert!(subscribe["id"].as_str().is_some());

    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/search/torrents?keyword=matrix",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = json_data(response).await;
    let torrents = data["items"].as_array().unwrap();
    assert_eq!(torrents.len(), 1);
    assert_eq!(
        torrents[0]["title"],
        "The.Matrix.1999.2160p.BluRay.x265-GROUP"
    );
    assert_eq!(torrents[0]["release"]["resolution"], "2160p");
    assert_eq!(torrents[0]["release"]["year"], 1999);
    assert_eq!(torrents[0]["release"]["confidence"], "high");
    assert!(
        data["sites"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["error"].is_null())
    );
}

#[tokio::test]
async fn search_reports_site_failures_without_dropping_hits() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let app = router(state(
        tmp.path(),
        fetcher,
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let site = create_site(&app).await;
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/search/torrents?keyword=matrix",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = json_data(response).await;
    let sites = data["sites"].as_array().unwrap();
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0]["site_id"], site["id"]);
    assert!(!sites[0]["error"].is_null());
    assert!(data["items"].as_array().unwrap().is_empty());
    assert!(!sites[0]["error"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn fetch_mode_selects_search_rss_or_both_on_run() {
    let cases = [
        ("search", vec!["search"]),
        ("rss", vec!["rss"]),
        ("both", vec!["search", "rss"]),
    ];
    for (fetch_mode, expected_modes) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let fetcher = Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::from([("search", nexusphp()), ("rss", rss_xml())]),
        });
        let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
        let app = router(state(tmp.path(), fetcher.clone(), downloader));
        create_site(&app).await;
        let subscribe = create_subscribe(&app, fetch_mode).await;

        let response = app
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
        assert_eq!(response.status(), StatusCode::OK, "{fetch_mode}");

        let modes: Vec<String> = fetcher
            .requests
            .lock()
            .iter()
            .map(|key| key.split_once(':').unwrap().0.to_string())
            .collect();
        assert_eq!(modes, expected_modes, "{fetch_mode}");
    }
}
