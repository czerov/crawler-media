use std::sync::{Arc, Mutex};
use api::{ApiState, PosterFetch, Store, router};
use axum::{body::Body, http::{Request, StatusCode}};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}
#[derive(Default)]
struct FreezeOnFetch(Mutex<Option<(ApiState, String)>>);
impl PosterFetch for FreezeOnFetch {
    fn get(&self, _: &str) -> Result<Vec<u8>, String> {
        if let Some((state, id)) = self.0.lock().unwrap().take() {
            api::library_lifecycle::start_library_removal(&state, &id).unwrap();
        }
        Ok(b"replacement".to_vec())
    }
}
fn fixture(root: &std::path::Path) -> (ApiState, String, MediaId) {
    let state = ApiState::new(Store::open(root.join("data")).unwrap(), "secret".into(),
        ProfileSet::load(None).unwrap(), Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))), root.join("library")).unwrap();
    let directory = root.join("extra");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("movie.strm"), b"video").unwrap();
    std::fs::write(directory.join("poster.jpg"), b"original").unwrap();
    let binding = state.store();
    let store = binding.lock();
    let library = store.create_library(MediaKind::Movie, "Extra", &[directory.to_str().unwrap()],
        "everyone", true, &[]).unwrap();
    let media = Media { id: MediaId::new(), kind: MediaKind::Movie, title: "Original".into(),
        year: None, original_title: None, tmdb_id: None, douban_id: None, tvdb_id: None,
        bangumi_id: None, anilist_id: None };
    store.insert_media(&media).unwrap();
    store.insert_ledger(&LedgerRow { id: LedgerId::new(), media_id: media.id,
        path: directory.join("movie.strm").display().to_string(), season: None, episode: None,
        resolution: None, codec: None, hdr: None, quality_source: QualitySource::Release,
        confidence: Confidence::High, filter_score: None }).unwrap();
    drop(store);
    (state, library.id, media.id)
}
fn request(id: &str, media: MediaId, action: &str, body: Value) -> Request<Body> {
    Request::builder().method("POST")
        .uri(format!("/api/v1/libraries/{id}/items/{media}/artwork/{action}"))
        .header("Authorization", "Bearer secret").header("Content-Type", "application/json")
        .body(Body::from(body.to_string())).unwrap()
}
async fn assert_deleting(response: axum::response::Response) {
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"]["code"], "library.deleting");
}
#[tokio::test]
async fn frozen_artwork_mutations_preserve_sidecar_and_manual_lock() {
    let temp = tempfile::tempdir().unwrap();
    let (state, id, media) = fixture(temp.path());
    let key = format!("artwork.{media}");
    let lock = json!({"poster_locked":true,"source":"existing"}).to_string();
    state.store().lock().put_setting(&key, &lock).unwrap();
    state.store().lock().begin_library_removal(&id).unwrap();
    let app = router(state.clone());
    for (action, body) in [("select", json!({"file_path":"https://offline/image"})),
        ("select", json!({"file_path":null})),
        ("upload", json!({"data_url":"data:image/jpeg;base64,bmV3"}))] {
        assert_deleting(app.clone().oneshot(request(&id, media, action, body)).await.unwrap()).await;
    }
    assert_eq!(std::fs::read(temp.path().join("extra/poster.jpg")).unwrap(), b"original");
    assert_eq!(state.store().lock().get_setting(&key).unwrap(), Some(lock));
}
#[tokio::test]
async fn selection_frozen_during_fetch_does_not_publish_bytes_or_lock() {
    let temp = tempfile::tempdir().unwrap();
    let (state, id, media) = fixture(temp.path());
    let fetch = Arc::new(FreezeOnFetch::default());
    let state = state.with_poster_fetch(fetch.clone());
    *fetch.0.lock().unwrap() = Some((state.clone(), id.clone()));
    let response = router(state.clone()).oneshot(request(&id, media, "select",
        json!({"file_path":"https://offline/image"}))).await.unwrap();
    assert_deleting(response).await;
    assert_eq!(std::fs::read(temp.path().join("extra/poster.jpg")).unwrap(), b"original");
    assert!(state.store().lock().get_setting(&format!("artwork.{media}")).unwrap().is_none());
}
#[tokio::test]
async fn active_artwork_upload_and_selection_keep_manual_lock_contract() {
    let temp = tempfile::tempdir().unwrap();
    let (state, id, media) = fixture(temp.path());
    let state = state.with_poster_fetch(Arc::new(FreezeOnFetch::default()));
    let app = router(state.clone());
    for (action, body, expected) in [
        ("upload", json!({"data_url":"data:image/jpeg;base64,bmV3"}), b"new".as_slice()),
        ("select", json!({"file_path":"https://offline/image"}), b"replacement".as_slice())] {
        let response = app.clone().oneshot(request(&id, media, action, body)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(std::fs::read(temp.path().join("extra/poster.jpg")).unwrap(), expected);
        let raw = state.store().lock().get_setting(&format!("artwork.{media}")).unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Value>(&raw).unwrap()["poster_locked"], true);
    }
}
