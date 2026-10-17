use std::collections::HashMap;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use domain::{Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use rusqlite::Connection;
use serde_json::json;
use tower::ServiceExt;

use super::common::{Fixtures, request, state, subscribe_payload};

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ))
}

fn rows(tmp: &tempfile::TempDir, db: &str, table: &str) -> i64 {
    Connection::open(tmp.path().join("data").join(db))
        .unwrap()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[tokio::test]
async fn invalid_self_interval_does_not_insert_media() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let response = app
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "media": { "kind": "movie", "title": "Atomic Self" },
                "coverage": { "kind": "movie" },
                "search_interval_secs": 60
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(rows(&tmp, "app.db", "media"), 0);
    assert_eq!(rows(&tmp, "subscribe.db", "subscribes"), 0);
}

#[tokio::test]
async fn invalid_legacy_downloader_does_not_insert_media_or_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let mut body = subscribe_payload("search");
    body["media"]["title"] = json!("Atomic Legacy");
    body["downloader_id"] = json!("bad-id");
    let before_filters = rows(&tmp, "app.db", "filters");
    let response = app
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(rows(&tmp, "app.db", "media"), 0);
    assert_eq!(rows(&tmp, "app.db", "filters"), before_filters);
}

#[tokio::test]
async fn failed_job_seed_rolls_back_legacy_and_self_resources() {
    for legacy in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let app = app(&tmp);
        let jobs = Connection::open(tmp.path().join("data/jobs.db")).unwrap();
        let before_jobs = rows(&tmp, "jobs.db", "job_defs");
        jobs.execute_batch("CREATE TRIGGER block_subscribe_search BEFORE INSERT ON job_defs WHEN NEW.kind = 'subscribe_search' BEGIN SELECT RAISE(FAIL, 'blocked'); END;").unwrap();
        let before_filters = rows(&tmp, "app.db", "filters");
        let (uri, body) = (
            "/api/v1/subscriptions",
            if legacy {
                subscribe_payload("search")
            } else {
                json!({
                    "media": { "kind": "movie", "title": "Atomic Self" },
                    "coverage": { "kind": "movie" }
                })
            },
        );
        let response = app
            .oneshot(request("POST", uri, Some("management-secret"), body))
            .await
            .unwrap();
        assert!(
            response.status().is_server_error(),
            "{}: {}",
            uri,
            response.status()
        );
        assert_eq!(rows(&tmp, "app.db", "media"), 0);
        assert_eq!(rows(&tmp, "app.db", "filters"), before_filters);
        assert_eq!(rows(&tmp, "subscribe.db", "subscribes"), 0);
        assert_eq!(rows(&tmp, "jobs.db", "job_defs"), before_jobs);
    }
}

#[tokio::test]
async fn failed_catalog_seed_removes_search_job_and_restores_existing_media() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let store = Store::open(tmp.path().join("data")).unwrap();
    let previous = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Old title".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("atomic-tmdb".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&previous).unwrap();
    let before_jobs = rows(&tmp, "jobs.db", "job_defs");
    let jobs = Connection::open(tmp.path().join("data/jobs.db")).unwrap();
    jobs.execute_batch("CREATE TRIGGER block_catalog BEFORE INSERT ON job_defs WHEN NEW.kind = 'catalog_refresh' BEGIN SELECT RAISE(FAIL, 'blocked'); END;").unwrap();
    let response = app
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "media": { "kind": "movie", "title": "New title", "tmdb_id": "atomic-tmdb", "year": 2024 },
                "coverage": { "kind": "movie" }
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(rows(&tmp, "jobs.db", "job_defs"), before_jobs);
    assert_eq!(rows(&tmp, "subscribe.db", "subscribes"), 0);
    assert_eq!(rows(&tmp, "app.db", "media"), 1);
    assert_eq!(store.get_media(previous.id).unwrap(), Some(previous));
}
