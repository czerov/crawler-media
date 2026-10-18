//! Review L-10: a permanently failing deletion stops being retried after a
//! bounded number of attempts and stays visible with its last error.

use store::Store;

#[test]
fn permanently_failing_deletion_stops_retrying() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    store
        .enqueue_file_deletion("/nowhere/file.mkv", "wash_replaced")
        .unwrap();
    for _ in 0..store::MAX_FILE_DELETION_ATTEMPTS {
        store
            .record_file_deletion_failure("/nowhere/file.mkv", 0, "EACCES")
            .unwrap();
    }
    assert!(store.list_due_file_deletions(i64::MAX).unwrap().is_empty());
    let stuck = store.list_stuck_file_deletions().unwrap();
    assert_eq!(
        stuck,
        vec![("/nowhere/file.mkv".to_string(), Some("EACCES".to_string()))]
    );
}
