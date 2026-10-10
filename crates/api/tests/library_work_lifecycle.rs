use std::sync::Arc;
use api::library_lifecycle::LibraryWorkRegistry;
use parking_lot::Mutex;

#[tokio::test]
async fn freeze_cancels_existing_work_and_blocks_new_leases() {
    let temp = tempfile::tempdir().unwrap();
    let store = Mutex::new(store::Store::open(temp.path()).unwrap());
    let root = temp.path().join("extra");
    let library = store.lock().create_library(domain::MediaKind::Tv, "Extra",
        &[root.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let registry = Arc::new(LibraryWorkRegistry::default());
    let lease = registry.acquire(&store, &library.id).unwrap();
    registry.freeze(&store, &library.id).unwrap();
    assert!(lease.control().is_cancelled());
    assert!(registry.acquire(&store, &library.id).is_err());
    assert!(tokio::time::timeout(std::time::Duration::from_millis(10),
        registry.wait_idle(&library.id)).await.is_err());
    drop(lease);
    tokio::time::timeout(std::time::Duration::from_secs(1), registry.wait_idle(&library.id)).await.unwrap();
}
