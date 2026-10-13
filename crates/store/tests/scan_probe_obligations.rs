use domain::{Confidence, LedgerId, LedgerRow, MediaId, QualitySource};
use store::Store;

fn sample_row(path: &str) -> LedgerRow {
    LedgerRow {
        id: LedgerId::new(),
        media_id: MediaId::new(),
        path: path.to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    }
}

#[test]
fn insert_and_obligation_are_atomic() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();

    let row = sample_row("/movies/test.mkv");

    // Add trigger on library.db that fails on obligation insert
    let lib_conn = rusqlite::Connection::open(data_dir.join("library.db")).unwrap();
    lib_conn
        .execute(
            "CREATE TRIGGER fail_ob_insert BEFORE INSERT ON ledger_probe_obligations
             BEGIN
                 SELECT RAISE(ABORT, 'forced obligation fail');
             END;",
            [],
        )
        .unwrap();

    let res = store.insert_scanned_ledger(&row, 1000);
    assert!(res.is_err(), "atomic insert must fail due to trigger");

    // Ledger row must NOT exist because transaction rolled back
    assert!(store.get_ledger(&row.id.to_string()).unwrap().is_none());
}

#[test]
fn ledger_cleanup_removes_obligation_not_user_facts() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let row = sample_row("/movies/clean.mkv");
    store.insert_scanned_ledger(&row, 1000).unwrap();

    // Verify obligation exists
    let obs = store.scan_probe_obligations(10).unwrap();
    assert_eq!(obs.len(), 1);
    assert_eq!(obs[0].ledger_id, row.id);

    // Delete ledger path
    store.delete_ledger_path(&row.path).unwrap();

    // Ledger and obligation must both be gone
    assert!(store.get_ledger(&row.id.to_string()).unwrap().is_none());
    let obs_after = store.scan_probe_obligations(10).unwrap();
    assert!(obs_after.is_empty(), "obligation must be deleted on ledger cleanup");
}

#[test]
fn ack_removes_obligation() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let row = sample_row("/movies/ack.mkv");
    store.insert_scanned_ledger(&row, 1000).unwrap();

    assert_eq!(store.scan_probe_obligations(10).unwrap().len(), 1);
    store.ack_scan_probe_obligation(row.id).unwrap();
    assert_eq!(store.scan_probe_obligations(10).unwrap().len(), 0);
}
