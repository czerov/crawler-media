//! Library removal must not leave app-managed cover bytes behind.
use std::sync::Arc;

use api::{library_lifecycle::start_library_removal, management::ApiState};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("no network".into()))
    }
}

fn new_state(temp: &tempfile::TempDir, data_dir: &std::path::Path) -> ApiState {
    ApiState::new(
        store::Store::open(data_dir).unwrap(),
        "test-token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap()
}

fn tv_library(state: &ApiState, name: &str, root: &std::path::Path) -> String {
    std::fs::create_dir_all(root).unwrap();
    state
        .store()
        .lock()
        .create_library(
            domain::MediaKind::Tv,
            name,
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap()
        .id
}

async fn wait_succeeded(state: &ApiState, operation_id: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let operation = state
                .store()
                .lock()
                .library_removal(operation_id)
                .unwrap()
                .unwrap();
            if operation.state == "succeeded" {
                return;
            }
            assert_ne!(operation.state, "failed", "{:?}", operation.error);
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn removal_deletes_legacy_and_versioned_covers_of_the_removed_library_only() {
    let temp = tempfile::tempdir().unwrap();
    let state = new_state(&temp, &temp.path().join("data"));
    let removed = tv_library(&state, "Removed", &temp.path().join("tv_removed"));
    let kept = tv_library(&state, "Kept", &temp.path().join("tv_kept"));
    let covers = state.store().lock().library_covers_dir();
    std::fs::create_dir_all(&covers).unwrap();
    let legacy = covers.join(format!("library-{removed}.jpg"));
    let versioned = covers.join(format!("library-{removed}-7c1f.jpg"));
    let untouched = covers.join(format!("library-{kept}.jpg"));
    for path in [&legacy, &versioned, &untouched] {
        std::fs::write(path, b"cover").unwrap();
    }

    let removal = start_library_removal(&state, &removed).unwrap();
    wait_succeeded(&state, &removal.operation_id).await;

    assert!(
        !legacy.exists(),
        "legacy cover must be removed with the Library"
    );
    assert!(
        !versioned.exists(),
        "versioned cover must be removed with the Library"
    );
    assert!(untouched.exists(), "other Libraries keep their covers");
}

#[tokio::test]
async fn startup_reconciles_covers_left_by_an_interrupted_removal() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    let state = new_state(&temp, &data_dir);
    let removed = tv_library(&state, "Removed", &temp.path().join("tv_removed"));
    let _kept = tv_library(&state, "Kept", &temp.path().join("tv_kept"));
    let covers = state.store().lock().library_covers_dir();
    std::fs::create_dir_all(&covers).unwrap();
    let cover = covers.join(format!("library-{removed}.jpg"));
    std::fs::write(&cover, b"cover").unwrap();

    let removal = start_library_removal(&state, &removed).unwrap();
    wait_succeeded(&state, &removal.operation_id).await;
    assert!(!cover.exists());

    // Crash window: the DB commit succeeded but the unlink never happened.
    std::fs::write(&cover, b"cover").unwrap();
    drop(state);

    let restarted = new_state(&temp, &data_dir);
    assert!(
        !cover.exists(),
        "startup reconciliation must finish the interrupted cleanup"
    );
    assert!(restarted
        .store()
        .lock()
        .get_library(&removed)
        .unwrap()
        .is_none());
}
