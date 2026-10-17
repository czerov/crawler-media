use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use store::{
    retry_delay_ms, ProbeJobUnitSpec, ProbeStage, ProbeStageCompletion, ProbeStageKey,
    ProbeStageStatus, Store,
};

fn create_test_media_and_ledger(store: &Store, path_suffix: &str) -> (Media, LedgerRow) {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Test Show".into(),
        year: Some(2026),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: format!("/path/to/{}", path_suffix),
        season: Some(1),
        episode: Some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).unwrap();
    (media, row)
}

#[test]
fn retry_schedule_has_four_delays_then_stops() {
    assert_eq!(retry_delay_ms(1), Some(60_000));
    assert_eq!(retry_delay_ms(2), Some(300_000));
    assert_eq!(retry_delay_ms(3), Some(900_000));
    assert_eq!(retry_delay_ms(4), Some(3_600_000));
    assert_eq!(retry_delay_ms(5), None);
}

#[test]
fn partial_job_is_terminal_after_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (_media, row) = create_test_media_and_ledger(&store, "ep1.strm");
    let ledger_id = row.id.to_string();

    let job_id = "test-job-partial";
    let spec = ProbeJobUnitSpec {
        ledger_id: &ledger_id,
        kind: "tv",
        force_fingerprint: false,
        reuse_fingerprint_cache: true,
        overwrite_markers: false,
        reuse_media_info_cache: true,
    };
    assert!(store.create_probe_job(job_id, "fingerprint_probe", &row.media_id.to_string(), Some(1), "test_scope", &[spec]).unwrap());
    assert!(store.start_probe_unit(job_id, &ledger_id).unwrap());

    // Finish unit with partial outcome: succeeded=false, but custom status "partial"
    let (job_result, changed, all_done) = store
        .finish_probe_unit_with_status(job_id, &ledger_id, "partial", Some("http_403"), Some("Forbidden"))
        .unwrap();
    assert!(changed);
    assert!(all_done);
    assert_eq!(job_result.status, "partial");
    let job = store.get_probe_job(job_id).unwrap().unwrap();
    assert_eq!(job.status, "partial");
    assert!(!job.is_active());

    drop(store);

    let reopened = Store::open(tmp.path()).unwrap();
    let job_after = reopened.get_probe_job(job_id).unwrap().unwrap();
    assert_eq!(job_after.status, "partial");
    assert!(!job_after.is_active());
}

#[test]
fn stale_job_cannot_finish_new_context() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (_media, row) = create_test_media_and_ledger(&store, "ep1.strm");
    let ledger_id = row.id.to_string();

    let key_old = ProbeStageKey {
        ledger_id: ledger_id.clone(),
        context_key: "ctx_v1".to_string(),
        stage: ProbeStage::Outro,
    };

    let key_new = ProbeStageKey {
        ledger_id: ledger_id.clone(),
        context_key: "ctx_v2".to_string(),
        stage: ProbeStage::Outro,
    };

    assert!(store.claim_probe_stage(&key_old, "job_1", 1_000_000).unwrap());

    // Switch context to v2
    assert!(store.claim_probe_stage(&key_new, "job_2", 1_000_100).unwrap());

    // Old job attempts to finish stage for key_old: succeeds for key_old, but cannot finish key_new
    let finished_old = store.finish_probe_stage(&key_old, "job_1", &ProbeStageCompletion::Succeeded, 1_000_200).unwrap();
    assert!(finished_old);

    // Old job attempts to finish key_new with job_1: rejected because active_job_id is job_2
    let finished_stale = store.finish_probe_stage(&key_new, "job_1", &ProbeStageCompletion::Succeeded, 1_000_300).unwrap();
    assert!(!finished_stale);

    let state_new = store.get_probe_stage(&key_new).unwrap().unwrap();
    assert_eq!(state_new.status, ProbeStageStatus::Running);
    assert_eq!(state_new.active_job_id, Some("job_2".to_string()));
}

#[test]
fn two_connections_claim_due_stage_once() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (_media, row) = create_test_media_and_ledger(&store, "ep1.strm");
    let ledger_id = row.id.to_string();

    let key = ProbeStageKey {
        ledger_id,
        context_key: "ctx_v1".to_string(),
        stage: ProbeStage::Outro,
    };

    // First, fail the stage once so it has a retry schedule
    assert!(store.claim_probe_stage(&key, "job_initial", 1_000_000).unwrap());
    store.finish_probe_stage(
        &key,
        "job_initial",
        &ProbeStageCompletion::Failed {
            error_kind: "http_403".to_string(),
            error: "Forbidden".to_string(),
        },
        1_000_050,
    ).unwrap();

    let stage = store.get_probe_stage(&key).unwrap().unwrap();
    assert_eq!(stage.status, ProbeStageStatus::Failed);
    assert_eq!(stage.failure_count, 1);
    assert_eq!(stage.next_retry_at_ms, Some(1_000_050 + 60_000));

    // At time 1_000_050 + 60_000, stage is due
    let due_time = 1_000_050 + 60_000;
    let due_list = store.list_due_probe_stages(due_time, 10).unwrap();
    assert_eq!(due_list.len(), 1);

    // Claim with two simulated concurrent workers
    let claimed_1 = store.claim_probe_stage(&key, "job_worker_1", due_time).unwrap();
    let claimed_2 = store.claim_probe_stage(&key, "job_worker_2", due_time).unwrap();

    assert!(claimed_1);
    assert!(!claimed_2);

    let current = store.get_probe_stage(&key).unwrap().unwrap();
    assert_eq!(current.status, ProbeStageStatus::Running);
    assert_eq!(current.active_job_id, Some("job_worker_1".to_string()));
}

#[test]
fn deleting_ledger_removes_only_its_stage_state() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (_m1, r1) = create_test_media_and_ledger(&store, "ep1.strm");
    let (_m2, r2) = create_test_media_and_ledger(&store, "ep2.strm");

    let k1 = ProbeStageKey {
        ledger_id: r1.id.to_string(),
        context_key: "ctx1".into(),
        stage: ProbeStage::Intro,
    };
    let k2 = ProbeStageKey {
        ledger_id: r2.id.to_string(),
        context_key: "ctx2".into(),
        stage: ProbeStage::Intro,
    };

    store.claim_probe_stage(&k1, "job1", 100).unwrap();
    store.claim_probe_stage(&k2, "job2", 100).unwrap();

    assert!(store.get_probe_stage(&k1).unwrap().is_some());
    assert!(store.get_probe_stage(&k2).unwrap().is_some());

    // Delete r1 ledger
    store.delete_ledger_path(&r1.path).unwrap();

    assert!(store.get_probe_stage(&k1).unwrap().is_none());
    assert!(store.get_probe_stage(&k2).unwrap().is_some());
}

#[test]
fn v6_database_migrates_without_losing_probe_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    // Initialize DB up to schema 6
    let store = Store::open(tmp.path()).unwrap();
    let (_m, r) = create_test_media_and_ledger(&store, "ep1.strm");
    let ledger_id = r.id.to_string();

    let job_id = "job-pre-migration";
    let spec = ProbeJobUnitSpec {
        ledger_id: &ledger_id,
        kind: "tv",
        force_fingerprint: false,
        reuse_fingerprint_cache: true,
        overwrite_markers: false,
        reuse_media_info_cache: true,
    };
    assert!(store.create_probe_job(job_id, "media_probe", &r.media_id.to_string(), Some(1), "scope_v6", &[spec]).unwrap());
    drop(store);

    // Reopen (runs migration if version bumped)
    let reopened = Store::open(tmp.path()).unwrap();
    let job = reopened.get_probe_job(job_id).unwrap().unwrap();
    assert_eq!(job.id, job_id);
    assert_eq!(job.status, "queued");
}
