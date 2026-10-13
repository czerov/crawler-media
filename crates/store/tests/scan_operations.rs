use store::{ScanOperationStatus, Store};

#[test]
fn enqueue_scan_deduplicates_active_operations() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let op1 = store
        .enqueue_scan("lib1", "sig1", &[], 1000)
        .unwrap();
    assert_eq!(op1.status, ScanOperationStatus::Queued);
    assert_eq!(op1.generation, 1);

    // Enqueueing again returns the existing operation
    let op2 = store
        .enqueue_scan("lib1", "sig1", &[], 1050)
        .unwrap();
    assert_eq!(op1.id, op2.id);

    // Cancel operation
    assert!(store.cancel_scan(&op1.id, 1100).unwrap());
    let op_cancelled = store.scan_operation(&op1.id).unwrap().unwrap();
    assert_eq!(op_cancelled.status, ScanOperationStatus::Cancelled);

    // Enqueueing after cancel creates generation 2
    let op3 = store
        .enqueue_scan("lib1", "sig1", &[], 1200)
        .unwrap();
    assert_ne!(op1.id, op3.id);
    assert_eq!(op3.generation, 2);
}
