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

#[tokio::test]
async fn failed_removal_stays_frozen_on_resume_and_is_not_auto_succeeded() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    let state = ApiState::new(
        store::Store::open(&data_dir).unwrap(),
        "test-token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();
    let root = temp.path().join("extra");
    std::fs::create_dir_all(&root).unwrap();
    let library = state
        .store()
        .lock()
        .create_library(
            domain::MediaKind::Tv,
            "Extra",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let removal = state.store().lock().begin_library_removal(&library.id).unwrap();
    state
        .store()
        .lock()
        .set_removal_state(&removal.operation_id, "failed", Some("timeout"))
        .unwrap();

    drop(state);

    // 重新启动服务并恢复 removal
    let resumed_state = ApiState::new(
        store::Store::open(&data_dir).unwrap(),
        "test-token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();

    let current = resumed_state
        .store()
        .lock()
        .library_removal(&removal.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.state, "failed");
    assert!(!resumed_state.store().lock().library_accepts_work(&library.id).unwrap());
    assert!(resumed_state.store().lock().get_library(&library.id).unwrap().is_some());
}
