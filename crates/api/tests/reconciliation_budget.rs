use std::sync::Arc;
use std::time::Duration;

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::Value;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

fn make_state(root: &std::path::Path) -> ApiState {
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

fn seed_reconciliation_tree(state: &ApiState, tmp: &std::path::Path) -> (String, std::path::PathBuf) {
    let store = state.store();
    let guard = store.lock();
    let library_root = tmp.join("library_root");
    std::fs::create_dir_all(&library_root).unwrap();

    let library = guard
        .create_library(
            MediaKind::Movie,
            "ReconcileLib",
            &[library_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    // Create two media items with missing files in ledger sharing the tree
    let media_id1 = MediaId::new();
    let media1 = Media {
        id: media_id1,
        kind: MediaKind::Movie,
        title: "Reconcile Movie 1".into(),
        year: Some(2024),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    guard.insert_media(&media1).unwrap();

    let row1 = LedgerRow {
        id: LedgerId::new(),
        media_id: media_id1,
        path: library_root.join("OldDir/test_movie1.mkv").to_str().unwrap().into(),
        season: None,
        episode: None,
        resolution: Some("1080p".into()),
        codec: Some("h264".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
    };
    guard.insert_ledger(&row1).unwrap();

    let media_id2 = MediaId::new();
    let media2 = Media {
        id: media_id2,
        kind: MediaKind::Movie,
        title: "Reconcile Movie 2".into(),
        year: Some(2024),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    guard.insert_media(&media2).unwrap();

    let row2 = LedgerRow {
        id: LedgerId::new(),
        media_id: media_id2,
        path: library_root.join("OldDir/test_movie2.mkv").to_str().unwrap().into(),
        season: None,
        episode: None,
        resolution: Some("1080p".into()),
        codec: Some("h264".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
    };
    guard.insert_ledger(&row2).unwrap();

    // Put the files into a new shared directory (NewDir/test_movie1.mkv and NewDir/test_movie2.mkv)
    let new_dir = library_root.join("NewDir");
    std::fs::create_dir_all(&new_dir).unwrap();
    std::fs::write(new_dir.join("test_movie1.mkv"), b"video content 1").unwrap();
    std::fs::write(new_dir.join("test_movie2.mkv"), b"video content 2").unwrap();

    (library.id, library_root)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconciliation_scans_each_directory_once() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let (library_id, _root_dir) = seed_reconciliation_tree(&state, tmp.path());

    let app = router(state.clone());

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/libraries/{library_id}/path-reconciliation-preview"))
        .header("Authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let preview: Value = serde_json::from_slice(&body_bytes).unwrap();

    let missing = preview["data"]["missing"].as_array().expect("missing list");
    assert_eq!(missing.len(), 2, "both missing items must be reported");

    let m1 = missing.iter().find(|m| m["path"].as_str().unwrap().contains("test_movie1.mkv")).expect("missing item 1");
    let c1 = m1["candidates"].as_array().expect("candidates 1");
    assert_eq!(c1.len(), 1);
    assert!(c1[0].as_str().unwrap().contains("NewDir/test_movie1.mkv"));

    let m2 = missing.iter().find(|m| m["path"].as_str().unwrap().contains("test_movie2.mkv")).expect("missing item 2");
    let c2 = m2["candidates"].as_array().expect("candidates 2");
    assert_eq!(c2.len(), 1);
    assert!(c2[0].as_str().unwrap().contains("NewDir/test_movie2.mkv"));

    assert_eq!(preview["data"]["incomplete"], false);
    // Under library_root: 1 child directory ("NewDir") = 1 entry; inside NewDir: 2 files = 2 entries.
    // Total examined entries across the single-pass tree scan must be exactly 3.
    let examined = preview["data"]["examined"].as_u64().expect("examined count");
    assert_eq!(examined, 3, "directory tree must be scanned exactly once, examining exactly 3 entries");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconciliation_bounds_budget_and_depth() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("bounded_root");
    std::fs::create_dir_all(&root).unwrap();

    // Create 10 deep directories (exceeding depth 5)
    let mut deep = root.clone();
    for d in 1..=8 {
        deep = deep.join(format!("depth{d}"));
        std::fs::create_dir_all(&deep).unwrap();
    }
    std::fs::write(deep.join("deep_file.mp4"), b"deep").unwrap();

    let control = library::WorkControl::default();
    // Index with max_entries = 5 to verify incomplete flag
    let idx_small = api::library_reconciliation::index_basenames(
        &[root.clone()],
        &control,
        5,
    )
    .unwrap();

    assert!(idx_small.examined >= 5);
    assert_eq!(idx_small.incomplete, true);

    // Index with plenty entries: depth > 5 file should not be indexed
    let idx_full = api::library_reconciliation::index_basenames(
        &[root],
        &control,
        100_000,
    )
    .unwrap();
    assert_eq!(idx_full.incomplete, false);
    assert!(!idx_full.candidates.contains_key(std::ffi::OsStr::new("deep_file.mp4")));
}
