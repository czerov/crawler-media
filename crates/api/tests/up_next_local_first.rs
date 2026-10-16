use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use api::{ApiState, Store, catalog::Catalog, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media::CatalogHit;
use parking_lot::{Condvar, Mutex};
use serde_json::Value;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

#[derive(Default)]
struct Gate {
    entered: Mutex<bool>,
    changed: Condvar,
    released: Mutex<bool>,
    release_changed: Condvar,
}

impl Gate {
    fn wait_until_entered(&self) {
        let mut guard = self.entered.lock();
        self.changed.wait_for(&mut guard, Duration::from_secs(2));
        assert!(*guard, "catalog worker did not reach gate");
    }

    fn block_worker(&self) {
        *self.entered.lock() = true;
        self.changed.notify_all();
        let mut guard = self.released.lock();
        self.release_changed.wait_for(&mut guard, Duration::from_secs(2));
        assert!(*guard, "test did not release catalog gate");
    }

    fn release(&self) {
        *self.released.lock() = true;
        self.release_changed.notify_all();
    }
}

struct GatedCatalog {
    gate: Option<Arc<Gate>>,
    season_requests: Arc<std::sync::atomic::AtomicUsize>,
}

impl Catalog for GatedCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<CatalogHit>, String> { Ok(Vec::new()) }
    fn search_tv(&self, _query: &str) -> Result<Vec<CatalogHit>, String> { Ok(Vec::new()) }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> { Ok(Vec::new()) }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> { Ok(Vec::new()) }
    fn details(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Option<Media>, String> { Ok(None) }

    fn season_details(&self, _tmdb_id: &str, _season: u32) -> Result<Vec<media::EpisodeMeta>, String> {
        self.season_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(gate) = &self.gate { gate.block_worker(); }
        Err("season details unavailable".into())
    }
}


fn make_state(root: &Path, catalog: Arc<dyn Catalog>) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
    .with_catalog(catalog)
}
fn seed_media(store: &Store, root: &Path, kind: MediaKind, title: &str, id: &str, season: Option<u32>, episode: Option<u32>) -> MediaId {
    let path = root.join(format!("{title}.mkv"));
    std::fs::write(&path, b"video").unwrap();
    let media = store.ensure_media(Media {
        id: MediaId::new(), kind, title: title.into(), year: None, original_title: None,
        tmdb_id: Some(id.into()), douban_id: None, tvdb_id: None, bangumi_id: None, anilist_id: None,
    }).unwrap();
    store.insert_ledger(&LedgerRow {
        id: LedgerId::new(), media_id: media.id, path: path.display().to_string(), season, episode,
        resolution: None, codec: None, hdr: None, quality_source: QualitySource::Release,
        confidence: Confidence::High, filter_score: None,
    }).unwrap();
    media.id
}

async fn request_up_next(app: axum::Router) -> axum::response::Response {
    app.oneshot(Request::builder().uri("/api/v1/playback/up-next")
        .header("authorization", "Bearer management-secret").body(Body::empty()).unwrap())
        .await.unwrap()
}

#[tokio::test]
async fn movie_resume_never_requests_tv_season() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("app")).unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state = make_state(&tmp.path().join("app"), Arc::new(GatedCatalog {
        gate: None, season_requests: calls.clone(),
    }));
    let movie_id = {
        let store = state.store();
        let movie_id = seed_media(&store.lock(), &tmp.path().join("app"), MediaKind::Movie, "Film", "77", None, None);
        let user = store.lock().admin_user_id().unwrap();
        store.lock().upsert_unit(user, movie_id, -1, -1, 50, Some(false), None, Some(100), None, None, false, 1).unwrap();
        movie_id
    };
    let response = request_up_next(router(state)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["data"]["items"][0]["media_item_id"], movie_id.to_string());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn blocked_metadata_does_not_delay_local_up_next() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("app")).unwrap();
    let gate = Arc::new(Gate::default());
    let state = make_state(&tmp.path().join("app"), Arc::new(GatedCatalog {
        gate: Some(gate.clone()), season_requests: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    }));
    let tv_id = {
        let store = state.store();
        let tv_id = seed_media(&store.lock(), &tmp.path().join("app"), MediaKind::Tv, "Series", "77", Some(1), Some(1));
        let user = store.lock().admin_user_id().unwrap();
        store.lock().upsert_unit(user, tv_id, 1, 1, 50, Some(false), None, Some(100), None, None, false, 1).unwrap();
        tv_id
    };
    let task = tokio::spawn(request_up_next(router(state)));
    tokio::task::spawn_blocking({ let gate = gate.clone(); move || gate.wait_until_entered() }).await.unwrap();
    let response = tokio::time::timeout(Duration::from_millis(250), task).await;
    let ready = response.is_ok();
    gate.release();
    let response = response.expect("up-next must respond while metadata is gated").unwrap();
    assert!(ready, "local card response was blocked by catalog metadata");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["data"]["items"][0]["media_item_id"], tv_id.to_string());
}
/// 集名回落必须读该集自己的 `.nfo`（视频同名的兄弟文件），而不是视频文件本身。
#[tokio::test]
async fn tv_episode_nfo_title_is_served_locally() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("app");
    std::fs::create_dir_all(&root).unwrap();
    let state = make_state(&root, Arc::new(GatedCatalog {
        gate: None, season_requests: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    }));
    let tv_id = {
        let store = state.store();
        let tv_id = seed_media(&store.lock(), &root, MediaKind::Tv, "Series", "5150", Some(1), Some(1));
        std::fs::write(
            root.join("Series.nfo"),
            "<episodedetails><title>Pilot From Nfo</title></episodedetails>",
        )
        .unwrap();
        let user = store.lock().admin_user_id().unwrap();
        store.lock().upsert_unit(user, tv_id, 1, 1, 50, Some(false), None, Some(100), None, None, false, 1).unwrap();
        tv_id
    };
    let response = request_up_next(router(state)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["data"]["items"][0]["media_item_id"], tv_id.to_string());
    assert_eq!(body["data"]["items"][0]["episode_title"], "Pilot From Nfo");
}

/// 解析不出的季不得在每个请求上重新排补全（每次补全占满一个共享 search permit）。
#[tokio::test]
async fn unresolvable_season_is_enriched_at_most_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("app");
    std::fs::create_dir_all(&root).unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state = make_state(&root, Arc::new(GatedCatalog {
        gate: None, season_requests: calls.clone(),
    }));
    {
        let store = state.store();
        let tv_id = seed_media(&store.lock(), &root, MediaKind::Tv, "Cooldown", "9911", Some(1), Some(1));
        let user = store.lock().admin_user_id().unwrap();
        store.lock().upsert_unit(user, tv_id, 1, 1, 50, Some(false), None, Some(100), None, None, false, 1).unwrap();
    }
    let app = router(state);
    assert_eq!(request_up_next(app.clone()).await.status(), StatusCode::OK);
    for _ in 0..200 {
        if calls.load(std::sync::atomic::Ordering::SeqCst) > 0 { break; }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "first request must schedule exactly one enrichment"
    );
    assert_eq!(request_up_next(app).await.status(), StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "cooldown must suppress the re-scheduled enrichment"
    );
}

// The public HTTP endpoint assertions above deliberately exercise route auth and DTO shape.
