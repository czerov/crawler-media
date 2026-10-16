use std::sync::Arc;
use std::time::Duration;

use api::browse_file_read::BrowseFileRead;
use api::{ApiState, Store, router};
use store::UNIT_WHOLE;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use parking_lot::{Condvar, Mutex};
use serde_json::Value;
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

struct GatedFileRead {
    gate: Arc<Gate>,
}

impl BrowseFileRead for GatedFileRead {
    fn read_nfo(&self, path: &std::path::Path) -> Option<library::NfoMeta> {
        library::read_nfo(path)
    }

    fn metadata(&self, path: &std::path::Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }

    fn is_file(&self, path: &std::path::Path) -> bool {
        if path.file_name().and_then(|n| n.to_str()) == Some("poster.jpg") {
            self.gate.enter_and_wait();
        }
        path.is_file()
    }
}

fn make_state(root: &std::path::Path, gated: Arc<GatedFileRead>) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
    .with_browse_file_read(gated)
}

async fn wait_entered(gate: &Arc<Gate>) {
    let entered = tokio::time::timeout(Duration::from_secs(2), async {
        while !gate.entered() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(entered.is_ok(), "injected browse IO was never entered");
}
fn seed_movie(state: &ApiState, tmp: &std::path::Path) -> (domain::UserId, String, MediaId) {
    let store = state.store();
    let guard = store.lock();
    let user_id = guard
        .user_id_by_token("management-secret")
        .unwrap()
        .expect("admin user");
    let library_root = tmp.join("movies");
    std::fs::create_dir_all(&library_root).unwrap();
    let library = guard
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[library_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let movie_file = library_root.join("Test Movie (2024)/test.mp4");
    std::fs::create_dir_all(movie_file.parent().unwrap()).unwrap();
    std::fs::write(&movie_file, b"content").unwrap();
    std::fs::write(movie_file.parent().unwrap().join("poster.jpg"), b"fake poster").unwrap();

    let media_id = MediaId::new();
    let media = Media {
        id: media_id,
        kind: MediaKind::Movie,
        title: "Test Movie".into(),
        year: Some(2024),
        original_title: None,
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
        season: Some(1),
        episode: Some(1),
        resolution: Some("1080p".into()),
        codec: Some("h264".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
    };
    guard.insert_ledger(&row).unwrap();

    guard
        .set_unit_marks(
            user_id,
            media_id,
            UNIT_WHOLE,
            UNIT_WHOLE,
            None,
            Some(true),
            1000,
        )
        .unwrap();
    (user_id, library.id, media_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_gallery_and_episodes_do_not_hold_store() {
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::new());
    let gated_read = Arc::new(GatedFileRead { gate: gate.clone() });
    let state = make_state(tmp.path(), gated_read);
    let (_admin_user, library_id, media_id) = seed_movie(&state, tmp.path());
    let app = router(state.clone());

    // Request 1: GET /api/v1/playback/favorites/gallery
    let app_clone = app.clone();
    let gallery_handle = tokio::spawn(async move {
        let req = Request::builder()
            .method("GET")
            .uri("/api/v1/playback/favorites/gallery")
            .header("Authorization", "Bearer management-secret")
            .body(Body::empty())
            .unwrap();
        app_clone.oneshot(req).await.unwrap()
    });

    // Wait for the gallery request to enter the gated file read
    wait_entered(&gate).await;
    let _release = ReleaseOnDrop(gate.clone());

    // While gallery request is blocked by gated IO, verify Store lock can be acquired immediately
    {
        let store = state.store();
        assert!(
            store.try_lock().is_some(),
            "Store lock must not be held while favorites_gallery is performing filesystem checks"
        );
    }

    // Concurrent request: library episode endpoint completes while gallery is held
    let episode_req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/libraries/{library_id}/items/{media_id}/episodes"))
        .header("Authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let episode_resp = app.clone().oneshot(episode_req).await.unwrap();
    assert_eq!(episode_resp.status(), StatusCode::OK);
    let ep_bytes = to_bytes(episode_resp.into_body(), 1024 * 1024).await.unwrap();
    let ep_json: Value = serde_json::from_slice(&ep_bytes).unwrap();
    assert!(ep_json["data"].as_array().is_some_and(|arr| !arr.is_empty()));

    // Concurrent request: heartbeat / auth keeps responsive while gate is held
    let heartbeat_req = Request::builder()
        .method("GET")
        .uri("/api/v1/auth/me")
        .header("Authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let heartbeat_resp = app.clone().oneshot(heartbeat_req).await.unwrap();
    assert_eq!(heartbeat_resp.status(), StatusCode::OK);

    // Release gate and assert the real gallery payload
    gate.release();
    let gallery_response = gallery_handle.await.unwrap();
    assert_eq!(gallery_response.status(), StatusCode::OK);
    let body_bytes = to_bytes(gallery_response.into_body(), 1024 * 1024).await.unwrap();
    let gallery: Value = serde_json::from_slice(&body_bytes).unwrap();
    let items = gallery["data"].as_array().expect("gallery items array");
    assert_eq!(items.len(), 1, "real gallery payload must not be empty");
    assert_eq!(items[0]["title"], "Test Movie");
    let images = items[0]["images"].as_array().expect("images array");
    assert!(
        images.iter().any(|img| img["kind"] == "poster"),
        "gallery must include discovered poster"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn playback_item_observes_poster_without_holding_store() {
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::new());
    let gated_read = Arc::new(GatedFileRead { gate: gate.clone() });
    let state = make_state(tmp.path(), gated_read);
    let (_admin_user, library_id, media_id) = seed_movie(&state, tmp.path());
    let ledger_id = state.store().lock().list_ledger().unwrap()[0].id;
    let app = router(state.clone());
    let item_app = app.clone();
    let _release = ReleaseOnDrop(gate.clone());
    let item_handle = tokio::spawn(async move {
        let request = Request::builder()
            .uri(format!("/api/v1/playback/items/{media_id}"))
            .header("Authorization", "Bearer management-secret")
            .body(Body::empty())
            .unwrap();
        item_app.oneshot(request).await.unwrap()
    });

    wait_entered(&gate).await;
    let store_available = state.store().try_lock().is_some();
    if !store_available {
        gate.release();
    }
    assert!(store_available, "playback poster discovery must not hold Store");
    let auth_request = Request::builder()
        .uri("/api/v1/auth/me")
        .header("Authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let auth_response = tokio::time::timeout(
        Duration::from_secs(2),
        app.oneshot(auth_request),
    )
    .await
    .expect("authentication must complete while poster discovery is gated")
    .unwrap();
    assert_eq!(auth_response.status(), StatusCode::OK);

    gate.release();
    let response = item_handle.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let item: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(item["data"], serde_json::json!({
        "media_item_id": media_id.to_string(),
        "library_id": library_id,
        "kind": "movie",
        "title": "Test Movie",
        "year": 2024,
        "poster_url": format!("/posters/{ledger_id}"),
    }));
}
