//! HTTP mutations must reject paths outside strict Library ownership without
//! changing file bytes or the authoritative ledger.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use domain::MediaKind;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

struct MovieLibrary {
    app: axum::Router,
    id: String,
    root: PathBuf,
    source: PathBuf,
}

async fn movie_library(tmp: &tempfile::TempDir) -> MovieLibrary {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader));
    let store = Store::open(tmp.path().join("data")).unwrap();
    let library = store.default_library(MediaKind::Movie).unwrap().unwrap();
    assert!(library.is_default, "exercise the default Library fallback");
    let root = library.root_paths[0].clone();
    let source = root.join("Matrix.1999.mkv");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&source, b"matrix-source-bytes").unwrap();
    scan_and_wait(&app, &library.id).await;
    let rows = ledger(&app).await;
    assert_eq!(rows.as_array().unwrap().len(), 1, "{rows}");
    assert_eq!(rows[0]["path"], source.to_str().unwrap());
    assert_eq!(rows[0]["library_id"], library.id);
    MovieLibrary {
        app,
        id: library.id,
        root,
        source,
    }
}

async fn ledger(app: &axum::Router) -> Value {
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/ledger",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json_data(response).await
}

#[tokio::test]
async fn organize_rejects_missing_ancestor_parent_escape_without_side_effects() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = movie_library(&tmp).await;
    let before = ledger(&fixture.app).await;
    let missing = fixture.root.join("new");
    let outside = fixture.root.parent().unwrap().join("outside");
    // Preserve the original input: normalizing before HTTP would miss the bug
    // where a nonexistent ancestor makes the raw lexical prefix look owned.
    let destination = fixture.root.join("new/../../outside/file.mkv");
    assert!(!missing.exists());
    assert!(!outside.exists());

    let response = fixture
        .app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{}/organize", fixture.id),
            Some("management-secret"),
            json!({ "renames": [{
                "from": fixture.source.to_str().unwrap(),
                "to": destination.to_str().unwrap(),
            }] }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = json_data(response).await;
    assert_eq!(data["applied"], 0, "{data}");
    assert_eq!(data["errors"].as_array().unwrap().len(), 1, "{data}");
    assert!(data["skipped"].as_array().unwrap().is_empty(), "{data}");
    assert_eq!(
        std::fs::read(&fixture.source).unwrap(),
        b"matrix-source-bytes"
    );
    assert_eq!(ledger(&fixture.app).await, before, "ledger must not change");
    assert!(
        !missing.exists(),
        "must reject before creating missing ancestor"
    );
    assert!(!outside.exists(), "must not create the escaped directory");
    assert!(!outside.join("file.mkv").exists());
}

#[tokio::test]
async fn organize_rejects_unregistered_source_before_creating_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = movie_library(&tmp).await;
    let before = ledger(&fixture.app).await;
    let loose = fixture.root.join("loose.mkv");
    std::fs::write(&loose, b"unregistered").unwrap();
    let destination = fixture.root.join("new/deep/file.mkv");
    let response = fixture
        .app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{}/organize", fixture.id),
            Some("management-secret"),
            json!({"renames":[{
                "from":loose.to_str().unwrap(), "to":destination.to_str().unwrap()
            }]}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = json_data(response).await;
    assert_eq!(data["applied"], 0);
    assert_eq!(data["errors"].as_array().unwrap().len(), 1);
    assert_eq!(std::fs::read(loose).unwrap(), b"unregistered");
    assert!(!fixture.root.join("new").exists());
    assert_eq!(ledger(&fixture.app).await, before);
}

#[tokio::test]
async fn path_reconciliations_rejects_outside_missing_source_in_default_library() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = movie_library(&tmp).await;
    let outside = tmp.path().join("outside-library");
    let missing_source = outside.join("missing/Matrix.1999.mkv");
    assert!(!outside.exists());
    // Simulate a legacy ledger row with a missing external source. The target
    // exists inside the default Library, so only strict source ownership can
    // prevent that Library from adopting the external row.
    let store = Store::open(tmp.path().join("data")).unwrap();
    assert!(
        store
            .rename_ledger_path(
                fixture.source.to_str().unwrap(),
                missing_source.to_str().unwrap(),
            )
            .unwrap()
    );
    drop(store);
    let before = ledger(&fixture.app).await;
    assert_eq!(before[0]["path"], missing_source.to_str().unwrap());

    let response = fixture
        .app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{}/path-reconciliations", fixture.id),
            Some("management-secret"),
            json!({ "reconciliations": [{
                "from": missing_source.to_str().unwrap(),
                "to": fixture.source.to_str().unwrap(),
            }] }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = json_data(response).await;
    assert_eq!(data["reconciled"], 0, "{data}");
    assert_eq!(data["errors"].as_array().unwrap().len(), 1, "{data}");
    assert_eq!(
        ledger(&fixture.app).await,
        before,
        "external ledger row must remain"
    );
    assert_eq!(
        std::fs::read(&fixture.source).unwrap(),
        b"matrix-source-bytes"
    );
    assert!(!missing_source.exists());
    assert!(
        !outside.exists(),
        "rejection must not create external directories"
    );
}
