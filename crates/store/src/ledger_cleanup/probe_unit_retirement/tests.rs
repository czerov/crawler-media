use crate::Store;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};

fn fixture(store: &Store, root: &std::path::Path) -> (Media, LedgerRow, LedgerRow) {
    let library = store
        .create_library(
            MediaKind::Tv,
            "A",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    assert_eq!(library.kind, MediaKind::Tv);
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Example".into(),
        year: None,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let row = |episode: u32| LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: root
            .join(format!("E{episode:02}.strm"))
            .display()
            .to_string(),
        season: Some(1),
        episode: Some(episode),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    let first = row(1);
    let second = row(2);
    store.insert_ledger(&first).unwrap();
    store.insert_ledger(&second).unwrap();
    (media, first, second)
}

fn unit(ledger_id: &str) -> crate::ProbeJobUnitSpec<'_> {
    crate::ProbeJobUnitSpec {
        ledger_id,
        kind: "tv",
        force_fingerprint: false,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: false,
    }
}

#[test]
fn deleting_a_ledger_retires_its_unit_and_keeps_the_job_completable() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let (media, first, second) = fixture(&store, &temp.path().join("tv"));
    let first_id = first.id.to_string();
    let second_id = second.id.to_string();
    let units = [unit(&first_id), unit(&second_id)];
    store
        .create_probe_job(
            "job-1",
            "media_probe",
            &media.id.to_string(),
            Some(1),
            "ledger:first",
            &units,
        )
        .unwrap();
    store.start_probe_unit("job-1", &first_id).unwrap();

    store.delete_ledger_path(&first.path).unwrap();

    let remaining = store.probe_job_units("job-1").unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].ledger_id, second_id);
    let job = store.get_probe_job("job-1").unwrap().unwrap();
    assert_eq!(job.status, "running");
    assert_eq!((job.total, job.completed), (1, 0));

    // The surviving unit can still drive the job to completion.
    store
        .finish_probe_unit("job-1", &second_id, true, None)
        .unwrap();
    let job = store.get_probe_job("job-1").unwrap().unwrap();
    assert_eq!(job.status, "succeeded");
    assert_eq!((job.total, job.completed, job.succeeded), (1, 1, 1));
}

#[test]
fn deleting_the_last_ledger_terminalizes_the_job_instead_of_stranding_it() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let (media, first, _second) = fixture(&store, &temp.path().join("tv"));
    let first_id = first.id.to_string();
    let units = [unit(&first_id)];
    store
        .create_probe_job(
            "job-2",
            "media_probe",
            &media.id.to_string(),
            Some(1),
            "ledger:only",
            &units,
        )
        .unwrap();
    store.start_probe_unit("job-2", &first_id).unwrap();

    store.delete_ledger_path(&first.path).unwrap();

    assert!(store.probe_job_units("job-2").unwrap().is_empty());
    let job = store.get_probe_job("job-2").unwrap().unwrap();
    assert_eq!(job.status, "cancelled");
    assert!(!job.is_active());
    assert_eq!(job.total, 0);
    assert_eq!(job.error.as_deref(), Some("file_deleted"));
}

#[test]
fn a_late_unit_finish_does_not_resurrect_a_retired_job() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let (media, first, _second) = fixture(&store, &temp.path().join("tv"));
    let first_id = first.id.to_string();
    let units = [unit(&first_id)];
    store
        .create_probe_job(
            "job-4",
            "marker_refresh",
            &media.id.to_string(),
            Some(1),
            "marker:retired",
            &units,
        )
        .unwrap();
    store.start_probe_unit("job-4", &first_id).unwrap();
    store.delete_ledger_path(&first.path).unwrap();
    assert_eq!(
        store.get_probe_job("job-4").unwrap().unwrap().status,
        "cancelled"
    );

    // The worker still holds the in-flight unit and reports it afterwards.
    store
        .finish_probe_unit("job-4", &first_id, true, None)
        .unwrap();

    assert_eq!(
        store.get_probe_job("job-4").unwrap().unwrap().status,
        "cancelled"
    );
}

#[test]
fn opening_a_pre_fix_database_repairs_orphan_probe_units() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let store = Store::open(&data).unwrap();
    let (media, first, _second) = fixture(&store, &temp.path().join("tv"));
    let first_id = first.id.to_string();
    let units = [unit(&first_id)];
    store
        .create_probe_job(
            "job-3",
            "media_probe",
            &media.id.to_string(),
            Some(1),
            "ledger:legacy",
            &units,
        )
        .unwrap();
    store.start_probe_unit("job-3", &first_id).unwrap();
    // Simulate a database written by the previous release: the ledger row is
    // gone but its probe unit survived, and the schema version predates the
    // repair migration.
    store
        .library
        .execute("DELETE FROM ledger WHERE id = ?1", [&first_id])
        .unwrap();
    store
        .library
        .pragma_update(None, "user_version", 9)
        .unwrap();
    drop(store);

    let reopened = Store::open(&data).unwrap();
    assert!(reopened.probe_job_units("job-3").unwrap().is_empty());
    let job = reopened.get_probe_job("job-3").unwrap().unwrap();
    assert_eq!(job.status, "cancelled");
    assert_eq!(job.total, 0);
}

/// Two-unit job whose first unit is already terminal while the second is still
/// outstanding, so a later deletion of the second ledger retires the last
/// unfinished unit of an otherwise finished prefix.
///
/// Returns the store plus the two ledger paths, so a test can delete whichever
/// one it wants to retire.
fn prefix_job(
    root: &std::path::Path,
    kind: &str,
    job_id: &str,
    prefix_status: &str,
) -> (Store, String, String) {
    let store = Store::open(root.join("data")).unwrap();
    let (media, first, second) = fixture(&store, &root.join("tv"));
    let first_id = first.id.to_string();
    let second_id = second.id.to_string();
    let units = [unit(&first_id), unit(&second_id)];
    store
        .create_probe_job(
            job_id,
            kind,
            &media.id.to_string(),
            Some(1),
            &format!("ledger:{job_id}"),
            &units,
        )
        .unwrap();
    store.start_probe_unit(job_id, &first_id).unwrap();
    store
        .finish_probe_unit_with_status(
            job_id,
            &first_id,
            prefix_status,
            None,
            (prefix_status != "succeeded").then_some("prefix unit did not succeed"),
        )
        .unwrap();
    store.start_probe_unit(job_id, &second_id).unwrap();
    (store, first.path.clone(), second.path.clone())
}

#[test]
fn deleting_the_last_outstanding_unit_finishes_a_job_with_a_completed_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let (store, _first_path, second_path) =
        prefix_job(temp.path(), "media_probe", "job-5", "succeeded");

    store.delete_ledger_path(&second_path).unwrap();

    let job = store.get_probe_job("job-5").unwrap().unwrap();
    assert_eq!((job.total, job.completed, job.succeeded), (1, 1, 1));
    assert_eq!(job.status, "succeeded");
    assert!(!job.is_active());
    assert!(job.finished_at_ms.is_some());
}

#[test]
fn deleting_the_last_outstanding_unit_fails_a_job_with_a_failed_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let (store, _first_path, second_path) =
        prefix_job(temp.path(), "media_probe", "job-6", "failed");

    store.delete_ledger_path(&second_path).unwrap();

    let job = store.get_probe_job("job-6").unwrap().unwrap();
    assert_eq!((job.total, job.completed, job.failed), (1, 1, 1));
    assert_eq!(job.status, "failed");
    assert!(!job.is_active());
    assert!(job.finished_at_ms.is_some());
}

#[test]
fn deleting_the_last_outstanding_unit_partializes_a_job_with_a_partial_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let (store, _first_path, second_path) =
        prefix_job(temp.path(), "media_probe", "job-10", "partial");

    store.delete_ledger_path(&second_path).unwrap();

    let job = store.get_probe_job("job-10").unwrap().unwrap();
    assert_eq!((job.total, job.completed, job.failed), (1, 1, 0));
    assert_eq!(job.status, "partial");
    assert!(!job.is_active());
    assert!(job.finished_at_ms.is_some());
}

#[test]
fn deleting_the_last_outstanding_unit_keeps_a_finished_marker_refresh_running() {
    let temp = tempfile::tempdir().unwrap();
    let (store, _first_path, second_path) =
        prefix_job(temp.path(), "marker_refresh", "job-7", "succeeded");

    store.delete_ledger_path(&second_path).unwrap();

    // A marker refresh is published atomically with its results; the counters
    // are resynced so `complete_marker_refresh` can still publish, but the job
    // must not be declared succeeded before those results exist.
    let job = store.get_probe_job("job-7").unwrap().unwrap();
    assert_eq!((job.total, job.completed, job.succeeded), (1, 1, 1));
    assert_eq!(job.status, "running");
    assert!(job.is_active());
}

#[test]
fn deleting_the_last_outstanding_unit_fails_a_marker_refresh_with_a_failed_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let (store, _first_path, second_path) =
        prefix_job(temp.path(), "marker_refresh", "job-8", "failed");

    store.delete_ledger_path(&second_path).unwrap();

    let job = store.get_probe_job("job-8").unwrap().unwrap();
    assert_eq!((job.total, job.completed, job.failed), (1, 1, 1));
    assert_eq!(job.status, "failed");
    assert!(!job.is_active());
    assert!(job.finished_at_ms.is_some());
}

#[test]
fn retiring_a_finished_unit_leaves_a_job_with_outstanding_work_running() {
    let temp = tempfile::tempdir().unwrap();
    let (store, first_path, _second_path) =
        prefix_job(temp.path(), "media_probe", "job-9", "succeeded");

    // Retire the finished unit, not the outstanding one.
    store.delete_ledger_path(&first_path).unwrap();

    let job = store.get_probe_job("job-9").unwrap().unwrap();
    assert_eq!((job.total, job.completed), (1, 0));
    assert_eq!(job.status, "running");
    assert!(job.is_active());
    assert!(job.finished_at_ms.is_none());
}
