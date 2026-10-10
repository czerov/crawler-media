use std::sync::Arc;
use api::{management::ApiState, library_lifecycle::start_library_removal};
use downloader::MemoryDownloader;
use indexer::{Fetcher, FetchRequest, IndexerError, ProfileSet};

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("no network".into()))
    }
}

#[tokio::test]
async fn removal_waits_for_lease_before_cleaning_and_preserves_source() {
    let temp = tempfile::tempdir().unwrap();
    let state = ApiState::new(store::Store::open(temp.path().join("data")).unwrap(),
        "test-token".into(), ProfileSet::load(None).unwrap(), Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(temp.path().join("stage"))), temp.path().join("library")).unwrap();
    let root = temp.path().join("extra");
    std::fs::create_dir_all(&root).unwrap();
    let library = state.store().lock().create_library(domain::MediaKind::Tv, "Extra",
        &[root.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let lease = state.acquire_library_work(&library.id).unwrap();
    let removal = start_library_removal(&state, &library.id).unwrap();
    assert!(lease.control().is_cancelled());
    assert!(state.store().lock().get_library(&library.id).unwrap().is_some());
    drop(lease);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let current = state.store().lock().library_removal(&removal.operation_id).unwrap().unwrap();
            if current.state == "succeeded" { break; }
            assert_ne!(current.state, "failed", "{:?}", current.error);
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert!(root.is_dir());
}
