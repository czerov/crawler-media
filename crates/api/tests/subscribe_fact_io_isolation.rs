use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use api::browse_file_read::BrowseFileRead;
use api::{ApiState, PosterFetch, Store, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{
    Confidence, Coverage, FetchMode, FilterId, LedgerId, LedgerRow, Media, MediaId, MediaKind,
    QualitySource, Subscribe, SubscribeId, UserId,
};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use parking_lot::{Condvar, Mutex};
use store::{LibraryPathResolver, LibraryPathScope, StoreError};
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

struct Gate {
    entered: Mutex<bool>,
    entered_cv: Condvar,
    open: Mutex<bool>,
    open_cv: Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            entered: Mutex::new(false),
            entered_cv: Condvar::new(),
            open: Mutex::new(false),
            open_cv: Condvar::new(),
        }
    }

    fn enter_and_wait(&self) {
        *self.entered.lock() = true;
        self.entered_cv.notify_all();
        let mut open = self.open.lock();
        while !*open {
            self.open_cv.wait(&mut open);
        }
    }

    fn entered(&self) -> bool {
        *self.entered.lock()
    }

    fn release(&self) {
        *self.open.lock() = true;
        self.open_cv.notify_all();
    }
}

struct ReleaseOnDrop(Arc<Gate>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct GatedBrowseFileRead {
    gate: Arc<Gate>,
}

impl BrowseFileRead for GatedBrowseFileRead {
    fn read_nfo(&self, path: &Path) -> Option<library::NfoMeta> {
        library::read_nfo(path)
    }

    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        self.gate.enter_and_wait();
        path.is_file()
    }
}

fn make_state(root: &Path, files: Arc<GatedBrowseFileRead>) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
    .with_browse_file_read(files)
}

fn seed_subscription(state: &ApiState, tmp: &Path) -> (Subscribe, PathBuf) {
    let store = state.store();
    let guard = store.lock();
    let lib_dir = tmp.join("movies");
    std::fs::create_dir_all(&lib_dir).unwrap();
    let library = guard
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[lib_dir.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let movie_file = lib_dir.join("sub_movie.mkv");
    std::fs::write(&movie_file, b"content").unwrap();

    let media_id = MediaId::new();
    let media = Media {
        id: media_id,
        kind: MediaKind::Movie,
        title: "Subscribe Movie".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    guard.insert_media(&media).unwrap();

    let row = LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: movie_file.to_str().unwrap().into(),
        season: None,
        episode: None,
        resolution: Some("1080p".into()),
        codec: Some("h264".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(85),
    };
    guard.insert_ledger(&row).unwrap();

    let sub = Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: Some(domain::LibraryId::from_str(&library.id).unwrap()),
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };
    (sub, movie_file)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_subscribe_fact_read_keeps_heartbeat_responsive() {
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::new());
    let state = make_state(
        tmp.path(),
        Arc::new(GatedBrowseFileRead {
            gate: gate.clone(),
        }),
    );
    let (subscribe, movie_file) = seed_subscription(&state, tmp.path());
    // Start the production facts read on a current-thread blocking worker
    let state_clone = state.clone();
    let sub_clone = subscribe.clone();
    let facts_handle = tokio::task::spawn_blocking(move || {
        api::read_subscribe_library_facts(&state_clone, &sub_clone, MediaKind::Movie)
    });

    // Wait for the gate to be entered in the production facts reading seam
    let entered = tokio::time::timeout(Duration::from_secs(2), async {
        while !gate.entered() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(entered.is_ok(), "injected facts read IO was never entered");
    let _release = ReleaseOnDrop(gate.clone());

    // Prove the Store lock is free while the read is blocked
    {
        let store = state.store();
        assert!(
            store.try_lock().is_some(),
            "Store lock must be free while out-of-lock facts evaluation is blocked"
        );
    }

    // Prove an auth/heartbeat request completes while the facts read is blocked
    let app = router(state.clone());
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/auth/me")
        .header("Authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Release the gate and assert the facts result
    gate.release();
    let facts_res = facts_handle.await.unwrap().expect("facts read result");
    let movie_fact = facts_res.movie().expect("movie fact should be discovered");
    assert_eq!(movie_fact.path.as_deref(), Some(movie_file.to_str().unwrap()));
    assert_eq!(movie_fact.score, 85);
}

struct FailingResolver {
    failing_path: PathBuf,
}

impl LibraryPathResolver for FailingResolver {
    fn resolve_path(&self, path: &Path) -> Result<PathBuf, StoreError> {
        if path.starts_with(&self.failing_path) {
            Err(StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "permission denied",
            )))
        } else {
            Ok(path.to_path_buf())
        }
    }
}

#[test]
fn failing_resolver_propagates_permission_denied() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let broader_root = tmp.path().join("media");
    let deeper_root = broader_root.join("special");
    std::fs::create_dir_all(&deeper_root).unwrap();

    let broader_lib = store
        .create_library(
            MediaKind::Movie,
            "Broader",
            &[broader_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let _deeper_lib = store
        .create_library(
            MediaKind::Movie,
            "Deeper",
            &[deeper_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let ownership = store.library_ownership().unwrap();
    let resolver = FailingResolver {
        failing_path: deeper_root.clone(),
    };

    // An unresolvable deeper root must return PermissionDenied error rather than
    // silently skipping and letting the broader root claim ownership.
    let res = LibraryPathScope::resolve_with(ownership, &resolver);
    match res {
        Err(StoreError::Io(err)) => {
            assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        }
        Err(other) => panic!("expected PermissionDenied IO error, got {other:?}"),
        Ok(scope) => {
            let candidate = deeper_root.join("movie.mkv");
            panic!(
                "failing resolver must propagate error rather than letting broader root take ownership: {:?}",
                scope.owner(&candidate, MediaKind::Movie)
            );
        }
    }
}
