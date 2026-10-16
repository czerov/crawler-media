use store::{ProbeJobUnitSpec, Store};

fn finish_workload(units: usize) -> (usize, usize, usize, usize, i64) {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let ledger_ids: Vec<_> = (0..units).map(|index| format!("ledger-{index}")).collect();
    let specs: Vec<_> = ledger_ids
        .iter()
        .map(|ledger_id| ProbeJobUnitSpec {
            ledger_id,
            kind: "movie",
            force_fingerprint: false,
            reuse_fingerprint_cache: false,
            overwrite_markers: false,
            reuse_media_info_cache: false,
        })
        .collect();
    store
        .create_probe_job("job", "scan", "media", None, "scope", &specs)
        .unwrap();
    let mut vm_steps = 0;
    let mut last_job = None;
    for ledger_id in &ledger_ids {
        let (job, changed, _, steps) = store
            .finish_probe_unit_with_status_and_vm_steps(
                "job", ledger_id, "succeeded", None, None,
            )
            .unwrap();
        assert!(changed);
        vm_steps += steps;
        last_job = Some(job);
    }
    let duplicate = store
        .finish_probe_unit_with_status("job", &ledger_ids[0], "failed", None, None)
        .unwrap();
    assert!(!duplicate.1);
    let job = last_job.unwrap();
    (job.completed, job.succeeded, job.failed, job.partial, vm_steps)
}

#[test]
fn large_probe_job_finish_work_is_near_linear() {
    let (completed_100, succeeded_100, failed_100, partial_100, vm_for_100) = finish_workload(100);
    let (completed_1_000, succeeded, failed, partial, vm_for_1_000) = finish_workload(1_000);
    assert_eq!(completed_100, 100);
    assert_eq!(succeeded_100 + failed_100 + partial_100, completed_100);
    assert_eq!(succeeded + failed + partial, completed_1_000);
    assert!(vm_for_1_000 <= vm_for_100 * 15);
}

#[test]
fn transitions_reconcile_and_reject_late_messages() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let specs = ["success", "partial", "failed", "cancelled"].map(|ledger_id| ProbeJobUnitSpec {
        ledger_id,
        kind: "movie",
        force_fingerprint: false,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: false,
    });
    store.create_probe_job("job", "scan", "media", None, "scope", &specs).unwrap();
    store.finish_probe_unit_with_status("job", "success", "succeeded", None, None).unwrap();
    store.finish_probe_unit_with_status("job", "partial", "partial", None, None).unwrap();
    store.finish_probe_unit_with_status("job", "failed", "failed", None, Some("failure")).unwrap();
    store.cancel_probe_job("job", "cancelled").unwrap();
    let before = store.get_probe_job("job").unwrap().unwrap();
    let late = store.finish_probe_unit_with_status("job", "cancelled", "succeeded", None, None).unwrap();
    assert!(!late.1);
    assert_eq!(late.0.status, "cancelled");
    assert_eq!(store.get_probe_job("job").unwrap().unwrap().completed, before.completed);
    let library_path = store.db_paths().into_iter().find(|path| path.ends_with("library.db")).unwrap();
    let connection = rusqlite::Connection::open(library_path).unwrap();
    connection.execute(
        "UPDATE probe_jobs SET completed=0, succeeded=0, failed=0, partial=0 WHERE id='job'",
        [],
    ).unwrap();
    drop(connection);
    let reconciled = store.reconcile_probe_job_counts("job").unwrap();
    assert_eq!((reconciled.completed, reconciled.succeeded, reconciled.failed, reconciled.partial), (4, 1, 1, 1));
    assert_eq!(reconciled.status, "cancelled");
}
#[test]
fn public_reconciliation_repairs_corrupted_counts() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let specs = ["one", "two"].map(|ledger_id| ProbeJobUnitSpec {
        ledger_id,
        kind: "movie",
        force_fingerprint: false,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: false,
    });
    store.create_probe_job("repair", "scan", "media", None, "repair", &specs).unwrap();
    store.finish_probe_unit_with_status("repair", "one", "partial", None, None).unwrap();
    let library_path = store.db_paths().into_iter().find(|path| path.ends_with("library.db")).unwrap();
    let connection = rusqlite::Connection::open(library_path).unwrap();
    connection.execute(
        "UPDATE probe_jobs SET completed=0, succeeded=0, failed=0, partial=0 WHERE id='repair'",
        [],
    ).unwrap();
    drop(connection);
    let repaired = store.reconcile_probe_job_counts("repair").unwrap();
    assert_eq!((repaired.completed, repaired.succeeded, repaired.failed, repaired.partial), (1, 0, 0, 1));
    assert_eq!(repaired.status, "running");
}

#[test]
fn startup_recovery_reconciles_active_job_counts_once() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let specs = ["done", "pending"].map(|ledger_id| ProbeJobUnitSpec {
        ledger_id,
        kind: "movie",
        force_fingerprint: false,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: false,
    });
    store.create_probe_job("recover", "scan", "media", None, "recover", &specs).unwrap();
    store.finish_probe_unit_with_status("recover", "done", "partial", None, None).unwrap();
    let library_path = store.db_paths().into_iter().find(|path| path.ends_with("library.db")).unwrap();
    let connection = rusqlite::Connection::open(library_path).unwrap();
    connection.execute("UPDATE probe_jobs SET completed=0, partial=0 WHERE id='recover'", []).unwrap();
    drop(connection);
    let recovered = store.recover_probe_jobs().unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!((recovered[0].0.completed, recovered[0].0.partial), (1, 1));
    assert_eq!(recovered[0].1.iter().find(|unit| unit.ledger_id == "done").unwrap().status, "partial");
}

/// 台账行被删除后，退役的 `partial` 单元不得再计入任务计数。
#[test]
fn retiring_a_partial_unit_keeps_the_job_counters_honest() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "Target".into(),
        year: None,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let rows = ["a", "b"].map(|suffix| domain::LedgerRow {
        id: domain::LedgerId::new(),
        media_id: media.id,
        path: format!("/library/{suffix}.mkv"),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: domain::QualitySource::Probe,
        confidence: domain::Confidence::High,
        filter_score: None,
    });
    for row in &rows {
        store.insert_ledger(row).unwrap();
    }
    let ids: Vec<String> = rows.iter().map(|row| row.id.to_string()).collect();
    let specs: Vec<_> = ids
        .iter()
        .map(|ledger_id| ProbeJobUnitSpec {
            ledger_id,
            kind: "movie",
            force_fingerprint: false,
            reuse_fingerprint_cache: false,
            overwrite_markers: false,
            reuse_media_info_cache: false,
        })
        .collect();
    store.create_probe_job("job", "scan", "media", None, "scope", &specs).unwrap();
    let (job, changed, _, _) = store
        .finish_probe_unit_with_status_and_vm_steps("job", &ids[0], "partial", None, None)
        .unwrap();
    assert!(changed);
    assert_eq!(job.partial, 1);

    // 台账行消失 -> 该单元退役，任务仍在跑。
    store.delete_ledger_path(&rows[0].path).unwrap();

    let (job, changed, _, _) = store
        .finish_probe_unit_with_status_and_vm_steps("job", &ids[1], "succeeded", None, None)
        .unwrap();
    assert!(changed);
    assert_eq!(job.partial, 0, "retired partial unit must stop being counted");
    assert_eq!(
        (job.total, job.completed, job.succeeded, job.failed),
        (1, 1, 1, 0)
    );
    assert_eq!(job.status, "succeeded");
    assert_eq!(job.succeeded + job.failed + job.partial, job.completed);
}
