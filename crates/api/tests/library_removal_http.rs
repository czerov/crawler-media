use std::sync::Arc;
use api::{management::ApiState, router};
use axum::{body::Body, http::{Request, StatusCode}};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use tower::ServiceExt;
struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> { Err(IndexerError::Fetch("offline".into())) }
}
fn request(method: &str, path: &str) -> Request<Body> {
    Request::builder().method(method).uri(path).header("Authorization", "Bearer secret").body(Body::empty()).unwrap()
}
#[tokio::test]
async fn delete_returns_operation_and_status_then_preserves_files() {
    let temp = tempfile::tempdir().unwrap();
    let state = ApiState::new(store::Store::open(temp.path().join("data")).unwrap(), "secret".into(),
        ProfileSet::load(None).unwrap(), Arc::new(NoFetch), Arc::new(MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library")).unwrap();
    let root = temp.path().join("extra");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("movie.mkv"), b"unchanged").unwrap();
    let library = state.store().lock().create_library(domain::MediaKind::Movie, "Extra", &[root.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let app = router(state.clone());
    let response = app.clone().oneshot(request("DELETE", &format!("/api/v1/libraries/{}", library.id))).await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let id = body["data"]["operation_id"].as_str().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let response = app.clone().oneshot(request("GET", &format!("/api/v1/library-removals/{id}"))).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            if body["data"]["state"] == "succeeded" { break; }
            assert_ne!(body["data"]["state"], "failed");
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert_eq!(std::fs::read(root.join("movie.mkv")).unwrap(), b"unchanged");
}
