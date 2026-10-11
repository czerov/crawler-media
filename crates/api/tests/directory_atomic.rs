//! `PUT /api/v1/directory` must apply every app.db write atomically: a failing
//! later write must roll back the roots/naming/mode/watch writes before it.

use std::path::Path;
use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::MediaKind;
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use tower::ServiceExt;

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

fn state(root: &Path) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
}

fn request(body: Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri("/api/v1/directory")
        .header("content-type", "application/json")
        .header("authorization", "Bearer management-secret")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn app_connection(root: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(root.join("data").join("app.db")).unwrap()
}

fn payload(tmp: &tempfile::TempDir) -> Value {
    json!({
        "movie_root": tmp.path().join("new-movies").display().to_string(),
        "tv_root": tmp.path().join("new-tv").display().to_string(),
        "movie_naming": "{title}/{title}{ext}",
        "tv_naming": "{title}/Season {season}/{title}{ext}",
        "transfer_mode": "copy",
        "scrape": true,
        "watch_intake": tmp.path().join("intake").display().to_string(),
        "watch_inplace": tmp.path().join("inplace").display().to_string(),
    })
}

#[tokio::test]
async fn directory_tv_root_failure_rolls_back_movie_root() {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(tmp.path());
    let old_movie = state.store().lock().library_root(MediaKind::Movie).unwrap();
    let old_tv = state.store().lock().library_root(MediaKind::Tv).unwrap();
    app_connection(tmp.path())
        .execute_batch(
            "CREATE TRIGGER reject_tv BEFORE UPDATE OF path ON library_roots
             WHEN OLD.kind = 'tv' BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;",
        )
        .unwrap();

    let response = router(state.clone())
        .oneshot(request(payload(&tmp)))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        state.store().lock().library_root(MediaKind::Movie).unwrap(),
        old_movie,
        "movie root must not persist when the tv root write fails"
    );
    assert_eq!(
        state.store().lock().library_root(MediaKind::Tv).unwrap(),
        old_tv
    );
}

#[tokio::test]
async fn directory_scrape_config_failure_keeps_roots_mode_and_watch() {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(tmp.path());
    let old_movie = state.store().lock().library_root(MediaKind::Movie).unwrap();
    let old_tv = state.store().lock().library_root(MediaKind::Tv).unwrap();
    app_connection(tmp.path())
        .execute_batch(
            "CREATE TRIGGER reject_scrape BEFORE INSERT ON settings
             WHEN NEW.key = 'metadata.scrape'
             BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;",
        )
        .unwrap();

    let response = router(state.clone())
        .oneshot(request(payload(&tmp)))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let store = state.store();
    assert_eq!(store.lock().library_root(MediaKind::Movie).unwrap(), old_movie);
    assert_eq!(store.lock().library_root(MediaKind::Tv).unwrap(), old_tv);
    assert_eq!(store.lock().transfer_mode().unwrap(), "hardlink");
    assert_eq!(store.lock().watch_intake().unwrap(), None);
    assert_eq!(store.lock().watch_inplace().unwrap(), None);
}
