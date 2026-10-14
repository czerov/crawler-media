use std::path::PathBuf;

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

/// 增量扫描目标必须落库：单飞合并后的目标集在崩溃重排/重启后仍然生效。
#[test]
fn enqueue_scan_persists_and_merges_targets() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let show_a = PathBuf::from("/media/tv/Show A");
    let show_b = PathBuf::from("/media/tv/Show B");

    let op1 = store
        .enqueue_scan("lib1", "sig1", &[show_a.clone()], 1000)
        .unwrap();
    assert_eq!(op1.targets, vec![show_a.clone()]);

    // 单飞：新目标并进同一个 operation，且写进行里。
    let op2 = store
        .enqueue_scan("lib1", "sig1", &[show_a.clone(), show_b.clone()], 1050)
        .unwrap();
    assert_eq!(op1.id, op2.id);
    assert_eq!(op2.targets, vec![show_a.clone(), show_b.clone()]);
    let reloaded = store.scan_operation(&op1.id).unwrap().unwrap();
    assert_eq!(
        reloaded.targets,
        vec![show_a.clone(), show_b.clone()],
        "合并后的目标集必须持久化"
    );

    // 认领时也要带上目标集，worker 才能只扫这些子目录。
    let claimed = store.claim_next_queued_scan(&[]).unwrap().unwrap();
    assert_eq!(claimed.targets, vec![show_a, show_b]);
    let listed = store.scan_operations_for_library("lib1").unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].targets.len(), 2);
}
