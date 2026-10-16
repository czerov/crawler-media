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

#[test]
fn missing_configured_reconciliation_root_marks_index_incomplete() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("missing-root");
    let index = api::library_reconciliation::index_basenames(
        &[missing], &library::WorkControl::default(), 100,
    ).unwrap();
    assert!(index.incomplete, "an unavailable configured root was not examined");
}

#[tokio::test]
async fn housekeeping_uses_bounded_blocking_admission() {
    let tmp = tempfile::tempdir().unwrap();
    let mut state = make_state(tmp.path());
    let (library_id, root) = seed_reconciliation_tree(&state, tmp.path());
    let budget = Arc::new(api::blocking_budget::BlockingBudget::new(1));
    state = state.with_http_work(budget.clone());
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let occupied = tokio::spawn(async move {
        budget.run_to_completion(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        }).await.unwrap();
    });
    entered_rx.await.unwrap();
    let app = router(state.clone());
    let mut statuses = Vec::new();
    for (method, endpoint, body) in [
        ("POST", "path-reconciliations", serde_json::json!({"reconciliations": [{
            "from": root.join("OldDir/test_movie1.mkv"),
            "to": root.join("NewDir/test_movie1.mkv"),
        }]})),
        ("GET", "root-consolidation-preview", Value::Null),
    ] {
        let response = app.clone().oneshot(Request::builder()
            .method(method)
            .uri(format!("/api/v1/libraries/{library_id}/{endpoint}"))
            .header("Authorization", "Bearer management-secret")
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string())).unwrap()).await.unwrap();
        statuses.push((endpoint, response.status()));
    }
    release_tx.send(()).unwrap();
    occupied.await.unwrap();
    for (endpoint, status) in statuses {
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE,
            "{endpoint} must return service unavailable when blocking admission is full");
    }
    assert!(state.store().lock().ledger_by_path(
        root.join("OldDir/test_movie1.mkv").to_str().unwrap()).unwrap().is_some(),
        "rejected work must not mutate the ledger");
}

struct ReconciliationFileRead {
    store: Arc<parking_lot::Mutex<Store>>,
    library_id: String,
    change: &'static str,
    entered: std::sync::atomic::AtomicBool,
    unlocked: std::sync::atomic::AtomicBool,
}

impl api::browse_file_read::BrowseFileRead for ReconciliationFileRead {
    fn read_nfo(&self, path: &std::path::Path) -> Option<library::NfoMeta> {
        library::read_nfo(path)
    }

    fn metadata(&self, path: &std::path::Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }

    fn is_file(&self, path: &std::path::Path) -> bool {
        use std::sync::atomic::Ordering;
        self.entered.store(true, Ordering::SeqCst);
        if let Some(store) = self.store.try_lock() {
            self.unlocked.store(true, Ordering::SeqCst);
            let library = store.get_library(&self.library_id).unwrap().unwrap();
            match self.change {
                "roots" => {
                    let root = library.root_paths[0].to_str().unwrap();
                    let added = library.root_paths[0].join("added-root");
                    store.update_library(&self.library_id, None,
                        Some(&[root, added.to_str().unwrap()]), None).unwrap();
                    assert_eq!(store.get_library(&self.library_id).unwrap().unwrap().root_paths.len(), 2);
                }
                "freeze" => { store.begin_library_removal(&self.library_id).unwrap(); }
                "source" => {
                    let from = library.root_paths[0].join("OldDir/test_movie1.mkv");
                    let mut row = store.ledger_by_path(from.to_str().unwrap()).unwrap().unwrap();
                    store.delete_ledger_path(&row.path).unwrap();
                    row.id = LedgerId::new();
                    store.insert_ledger(&row).unwrap();
                }
                "unchanged" => {}
                _ => panic!("unexpected fixture change"),
            }
        }
        path.is_file()
    }
}

#[tokio::test]
async fn reconciliation_revalidates_off_lock_scope_freeze_and_source() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for change in ["unchanged", "roots", "freeze", "source"] {
        let tmp = tempfile::tempdir().unwrap();
        let state = make_state(tmp.path());
        let (library_id, root) = seed_reconciliation_tree(&state, tmp.path());
        let files = Arc::new(ReconciliationFileRead {
            store: state.store(), library_id: library_id.clone(), change,
            entered: AtomicBool::new(false), unlocked: AtomicBool::new(false),
        });
        let state = state.with_browse_file_read(files.clone());
        let from = root.join("OldDir/test_movie1.mkv");
        let to = root.join("NewDir/test_movie1.mkv");
        let response = router(state.clone()).oneshot(Request::builder()
            .method("POST")
            .uri(format!("/api/v1/libraries/{library_id}/path-reconciliations"))
            .header("Authorization", "Bearer management-secret")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::json!({"reconciliations": [{
                "from": from, "to": to,
            }]}).to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{change}");
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert!(files.entered.load(Ordering::SeqCst), "real FS seam: {change}");
        assert!(files.unlocked.load(Ordering::SeqCst), "FS must not hold Store: {change}");
        let applied = change == "unchanged";
        assert_eq!(body["data"]["reconciled"], usize::from(applied), "{change}: {body}");
        assert_eq!(body["data"]["errors"].as_array().unwrap().len(), usize::from(!applied));
        let store = state.store();
        let store = store.lock();
        assert_eq!(store.ledger_by_path(from.to_str().unwrap()).unwrap().is_some(), !applied);
        assert_eq!(store.ledger_by_path(to.to_str().unwrap()).unwrap().is_some(), applied);
    }
}
