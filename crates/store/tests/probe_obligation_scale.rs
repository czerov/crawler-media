use std::str::FromStr;

use domain::{Confidence, LedgerId, LedgerRow, MediaId, QualitySource};
use store::{ScanProbeObligation, Store};

fn library_schema_version(store: &Store) -> i64 {
    store
        .schema_versions()
        .into_iter()
        .find(|(name, _)| *name == "library")
        .map(|(_, version)| version)
        .expect("library schema version is reported")
}

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
fn probe_obligations_bounded_vm_steps_at_scale() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    // 100 fixed target obligations with low timestamps (1..=100)
    for index in 1..=100 {
        let row = sample_row(&format!("/target/{index}.mkv"));
        store.insert_scanned_ledger(&row, index).unwrap();
    }

    // 1k series baseline: 1,000 backlog items with higher timestamps
    for index in 101..=1_100 {
        let row = sample_row(&format!("/backlog/{index}.mkv"));
        store.insert_scanned_ledger(&row, 10_000 + index).unwrap();
    }

    let (obs_1k, vm_steps_1k) = store.scan_probe_obligations_with_vm_steps(100).unwrap();
    assert_eq!(obs_1k.len(), 100);
    assert_eq!(obs_1k[0].created_at_ms, 1);
    assert!(vm_steps_1k > 0, "vm_steps_1k must be > 0");

    // Grow backlog to 50,000 items (49,000 additional rows)
    for index in 1_101..=50_100 {
        let row = sample_row(&format!("/backlog/{index}.mkv"));
        store.insert_scanned_ledger(&row, 10_000 + index).unwrap();
    }

    let (obs_50k, vm_steps_50k) = store.scan_probe_obligations_with_vm_steps(100).unwrap();
    assert_eq!(obs_50k.len(), 100);
    assert_eq!(obs_50k, obs_1k);
    assert_eq!(obs_50k[0].created_at_ms, 1);
    assert!(
        vm_steps_50k <= vm_steps_1k * 2,
        "vm_steps_50k ({vm_steps_50k}) must be <= vm_steps_1k * 2 ({})",
        vm_steps_1k * 2
    );
}

#[test]
fn probe_obligations_limit_clamping() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    for index in 1..=150 {
        let row = sample_row(&format!("/clamp/{index}.mkv"));
        store.insert_scanned_ledger(&row, index).unwrap();
    }

    // limit 0 clamped to 1
    let clamped_zero = store.scan_probe_obligations(0).unwrap();
    assert_eq!(clamped_zero.len(), 1);
    assert_eq!(clamped_zero[0].created_at_ms, 1);

    // limit 101 clamped to 100
    let clamped_101 = store.scan_probe_obligations(101).unwrap();
    assert_eq!(clamped_101.len(), 100);
    assert_eq!(clamped_101[0].created_at_ms, 1);
}

#[test]
fn probe_obligations_migration_upgrade_and_idempotence() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");

    // 1. First open: creates schema v11 with index
    let store = Store::open(&data_dir).unwrap();
    assert_eq!(library_schema_version(&store), 11);

    // Seed durable data through public API before downgrade:
    // Include two obligations sharing one created_at_ms to exercise ledger_id tie order.
    let id_small = LedgerId::from_str("00000000-0000-4000-8000-000000000001").unwrap();
    let id_large = LedgerId::from_str("ffffffff-ffff-4fff-bfff-ffffffffffff").unwrap();
    let mut row_large = sample_row("/movie/seed_1.mkv");
    row_large.id = id_large;
    let mut row_small = sample_row("/movie/seed_2.mkv");
    row_small.id = id_small;
    let row3 = sample_row("/movie/seed_3.mkv");
    let expected_rows = vec![row_large.clone(), row_small.clone(), row3.clone()];

    store.insert_scanned_ledger(&row_large, 100).unwrap();
    store.insert_scanned_ledger(&row_small, 100).unwrap();
    store.insert_scanned_ledger(&row3, 200).unwrap();

    let expected_obs = vec![
        ScanProbeObligation {
            ledger_id: id_small,
            created_at_ms: 100,
            reason: "scan".to_string(),
        },
        ScanProbeObligation {
            ledger_id: id_large,
            created_at_ms: 100,
            reason: "scan".to_string(),
        },
        ScanProbeObligation {
            ledger_id: row3.id,
            created_at_ms: 200,
            reason: "scan".to_string(),
        },
    ];

    drop(store);

    // 2. Re-open is a no-op and preserves version 11
    let store = Store::open(&data_dir).unwrap();
    assert_eq!(library_schema_version(&store), 11);
    drop(store);

    // 3. Opening twice successively does not error
    let store1 = Store::open(&data_dir).unwrap();
    drop(store1);
    let store2 = Store::open(&data_dir).unwrap();
    drop(store2);

    // 4. Upgrade from v10: simulate v10 database
    let lib_conn = rusqlite::Connection::open(data_dir.join("library.db")).unwrap();
    lib_conn.pragma_update(None, "user_version", 10).unwrap();
    // Drop the index to simulate schema version 10
    lib_conn
        .execute(
            "DROP INDEX IF EXISTS ledger_probe_obligations_created_ledger_idx",
            [],
        )
        .unwrap();
    drop(lib_conn);

    // Helper to check version 11, index presence, ledger rows, and obligations
    let assert_state = |st: &Store| {
        assert_eq!(library_schema_version(st), 11);
        let conn = rusqlite::Connection::open(data_dir.join("library.db")).unwrap();
        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'ledger_probe_obligations_created_ledger_idx'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx_count, 1);

        let mut actual_rows = st.list_ledger().unwrap();
        actual_rows.sort_by_key(|r| r.path.clone());
        let mut exp_rows = expected_rows.clone();
        exp_rows.sort_by_key(|r| r.path.clone());
        assert_eq!(actual_rows, exp_rows);

        let actual_obs = st.scan_probe_obligations(100).unwrap();
        assert_eq!(actual_obs, expected_obs);
    };

    // Reopen runs migration, creates index idempotently, and bumps to 11
    let upgraded_store = Store::open(&data_dir).unwrap();
    assert_state(&upgraded_store);
    drop(upgraded_store);

    // Reopen again and assert state persists
    let reopened_store = Store::open(&data_dir).unwrap();
    assert_state(&reopened_store);
    drop(reopened_store);

    // 5. Older binary pre-check: a database with user_version > 11 must be refused
    let lib_conn = rusqlite::Connection::open(data_dir.join("library.db")).unwrap();
    lib_conn.pragma_update(None, "user_version", 12).unwrap();
    drop(lib_conn);

    let err = match Store::open(&data_dir) {
        Ok(_) => panic!("higher schema version must be refused"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("library"));
}
