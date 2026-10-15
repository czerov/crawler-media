// Worker-driven windows: shares the fixture in marker_refresh_lifecycle_tests.rs (same `include!`d module).
async fn wait_for_terminal(store: &Arc<Mutex<Store>>, job_id: &str) -> crate::store::ProbeJob {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let job = store.lock().get_probe_job(job_id).unwrap().unwrap();
            if !job.is_active() {
                return job;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("job must reach a persisted terminal state")
}
async fn wait_for_worker_cleanup(harness: &RefreshHarness) {
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let metadata_pending = harness
                .manager
                .metadata_pending
                .load(std::sync::atomic::Ordering::Acquire);
            let seen_len = harness.manager.seen.lock().len();
            let has_active_work = harness.registry.has_active_work();
            if metadata_pending == 0 && seen_len == 0 && !has_active_work {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await;
    if result.is_err() {
        let metadata_pending = harness
            .manager
            .metadata_pending
            .load(std::sync::atomic::Ordering::Acquire);
        let seen_keys: Vec<String> = harness.manager.seen.lock().iter().cloned().collect();
        let has_active_work = harness.registry.has_active_work();
        panic!(
            "wait_for_worker_cleanup timed out: metadata_pending={metadata_pending} (expected 0), seen={seen_keys:?} (expected empty), has_active_work={has_active_work} (expected false)"
        );
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn window1_queued_stage_freeze_aborts_cleanly_and_releases_scope() {
    let harness = RefreshHarness::new();
    assert_eq!(
        harness
            .manager
            .enqueue_forced_item_refresh(harness.make_units())
            .unwrap(),
        2
    );

    let active_job = harness
        .store
        .lock()
        .active_probe_job_for_scope(&harness.scope)
        .unwrap()
        .expect("刷新任务应当已入库持有 scope");

    harness
        .registry
        .freeze(&harness.store, &harness.library_id)
        .unwrap();

    assert!(harness.manager.try_start_workers());

    let job = wait_for_terminal(&harness.store, &active_job.id).await;
    assert_eq!(job.status, "cancelled");
    harness.assert_old_markers_intact();
    assert!(
        harness
            .store
            .lock()
            .active_probe_job_for_scope(&harness.scope)
        .unwrap()
        .is_none()
    );
    wait_for_worker_cleanup(&harness).await;
    assert!(!harness.registry.has_active_work());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn window2_execution_handover_freeze_prevents_publication() {
    let harness = RefreshHarness::new();
    assert_eq!(
        harness
            .manager
            .enqueue_forced_item_refresh(harness.make_units())
            .unwrap(),
        2
    );
    let job_id = harness
        .store
        .lock()
        .active_probe_job_for_scope(&harness.scope)
        .unwrap()
        .unwrap()
        .id;

    let first = harness
        .manager
        .metadata_rx
        .try_lock()
        .unwrap()
        .try_recv()
        .unwrap();
    let _second = harness
        .manager
        .metadata_rx
        .try_lock()
        .unwrap()
        .try_recv()
        .unwrap();

    let work = probe::FingerprintWork {
        unit: first,
        media_duration_ms: Some(1_200_000),
        source_version: "v1".into(),
        duration_secs: 180,
        start_job: false,
    };
    assert!(harness.manager.enqueue_fingerprint_after_metadata(work));

    harness
        .registry
        .freeze(&harness.store, &harness.library_id)
        .unwrap();

    assert!(harness.manager.try_start_workers());

    let job = wait_for_terminal(&harness.store, &job_id).await;
    assert_eq!(job.status, "cancelled");
    harness.assert_old_markers_intact();
    assert!(
        harness
            .store
            .lock()
            .active_probe_job_for_scope(&harness.scope)
            .unwrap()
            .is_none()
    );
    // This case drains the metadata queue by hand so only the fingerprint worker can
    // cancel the job; those two drained units never run, so `metadata_pending` stays at 2
    // and the only worker-owned state this scenario can observe is the registry.
    assert!(!harness.registry.has_active_work());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn window5_resource_lifecycle_releases_leases_counters_and_scope() {
    let harness = RefreshHarness::new();
    let units = harness.make_units();
    let lease = harness
        .registry
        .acquire(&harness.store, &harness.library_id)
        .unwrap();
    assert!(harness.registry.has_active_work());
    drop(lease);
    assert!(!harness.registry.has_active_work());

    assert_eq!(
        harness
            .manager
            .enqueue_forced_item_refresh(units)
            .unwrap(),
        2
    );
    assert_eq!(
        harness
            .manager
            .metadata_pending
            .load(std::sync::atomic::Ordering::Acquire),
        2
    );
    assert!(
        harness
            .manager
            .seen
            .lock()
            .contains(&harness.rows[0].id.to_string())
    );

    let job_id = harness
        .store
        .lock()
        .active_probe_job_for_scope(&harness.scope)
        .unwrap()
        .expect("刷新任务应当已入库持有 scope")
        .id;

    assert!(harness.manager.try_start_workers());

    let _job = wait_for_terminal(&harness.store, &job_id).await;
    wait_for_worker_cleanup(&harness).await;

    assert_eq!(
        harness
            .manager
            .metadata_pending
            .load(std::sync::atomic::Ordering::Acquire),
        0
    );
    assert!(harness.manager.seen.lock().is_empty());
    assert!(!harness.registry.has_active_work());
    assert!(
        harness
            .store
            .lock()
            .active_probe_job_for_scope(&harness.scope)
            .unwrap()
            .is_none()
    );
    assert!(!harness.manager.is_queued(&harness.rows[0].id.to_string()));
    assert!(!harness.manager.is_queued(&harness.rows[1].id.to_string()));
    harness.assert_old_markers_intact();

    // Scope is immediately re-acquirable without AlreadyRunning error
    assert_eq!(
        harness
            .manager
            .enqueue_forced_item_refresh(harness.make_units())
            .unwrap(),
        2
    );
}
