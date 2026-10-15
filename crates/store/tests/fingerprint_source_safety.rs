use domain::{Confidence, LedgerId, LedgerRow, MediaId, QualitySource};
use store::{
    MarkerResultReplacement, ProbeJobUnitSpec, Store, StoredFingerprintAttempt,
    StoredFingerprintSample, StoredMediaMarker,
};

fn row(root: &std::path::Path) -> LedgerRow {
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: MediaId::new(),
        path: root.join("episode.mkv").to_string_lossy().into_owned(),
        season: Some(1),
        episode: Some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    std::fs::write(&row.path, b"fixture").unwrap();
    row
}

fn marker(row: &LedgerRow, source: &str) -> StoredMediaMarker {
    StoredMediaMarker {
        media_id: row.media_id,
        season: 1,
        episode: 1,
        intro_start_ms: Some(10_000),
        intro_end_ms: Some(70_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: source.into(),
        locked: false,
        updated_at: 0,
    }
}

#[test]
fn checked_publication_rejects_file_only_deletion_and_keeps_previous_result() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let row = row(temp.path());
    store.insert_ledger(&row).unwrap();
    store.put_media_marker(&marker(&row, "old")).unwrap();
    let replacement = MarkerResultReplacement {
        media_id: row.media_id,
        season: 1,
        markers: vec![marker(&row, "new")],
        chapter_updates: vec![(row.id.to_string(), vec![])],
    };
    std::fs::remove_file(&row.path).unwrap();
    assert!(
        store
            .replace_marker_results_batch_checked(std::slice::from_ref(&replacement))
            .is_err()
    );
    assert_eq!(
        store
            .get_media_marker(row.media_id, Some(1), Some(1))
            .unwrap()
            .unwrap()
            .source,
        "old"
    );
    assert!(
        store
            .get_cached_chapters(&row.id.to_string())
            .unwrap()
            .is_none()
    );
    // A valid source still commits markers and chapters together.
    std::fs::write(&row.path, b"restored").unwrap();
    store
        .replace_marker_results_batch_checked(&[replacement])
        .unwrap();
    assert_eq!(
        store
            .get_media_marker(row.media_id, Some(1), Some(1))
            .unwrap()
            .unwrap()
            .source,
        "new"
    );
    assert_eq!(
        store.get_cached_chapters(&row.id.to_string()).unwrap(),
        Some(vec![])
    );
}

#[test]
fn guarded_sample_commit_never_recreates_deleted_ledger_samples() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let row = row(temp.path());
    store.insert_ledger(&row).unwrap();
    let (attempt, sample) = capture_result(&row);
    store.begin_fingerprint_attempt(&attempt).unwrap();
    assert!(
        !store
            .complete_fingerprint_attempt_for_source(&attempt, &sample, "wrong-path")
            .unwrap()
    );
    assert!(
        store
            .find_covering_fingerprint_samples(&sample_query(&sample))
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .complete_fingerprint_attempt_for_source(&attempt, &sample, &row.path)
            .unwrap()
    );
    assert!(
        !store
            .find_covering_fingerprint_samples(&sample_query(&sample))
            .unwrap()
            .is_empty()
    );
    store.delete_ledger_path(&row.path).unwrap();
    assert!(
        !store
            .complete_fingerprint_attempt_for_source(&attempt, &sample, &row.path)
            .unwrap()
    );
    assert!(
        store
            .find_covering_fingerprint_samples(&sample_query(&sample))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn batch_cancellation_is_terminal_preserves_completed_units_and_releases_scope() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let units: Vec<_> = ["first", "deleted", "pending"]
        .iter()
        .map(|id| ProbeJobUnitSpec {
            ledger_id: id,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: true,
            reuse_media_info_cache: true,
        })
        .collect();
    store
        .create_probe_job("job", "marker_refresh", "media", Some(1), "scope", &units)
        .unwrap();
    store.start_probe_unit("job", "first").unwrap();
    store.finish_probe_unit("job", "first", true, None).unwrap();
    store.start_probe_unit("job", "deleted").unwrap();
    store.cancel_probe_job("job", "file_deleted").unwrap();
    let job = store.get_probe_job("job").unwrap().unwrap();
    assert_eq!(job.status, "cancelled");
    assert_eq!((job.completed, job.succeeded, job.failed), (3, 1, 0));
    assert_eq!(job.error.as_deref(), Some("file_deleted"));
    assert!(job.finished_at_ms.is_some());
    let persisted = store.probe_job_units("job").unwrap();
    assert_eq!(
        persisted.iter().filter(|u| u.status == "succeeded").count(),
        1
    );
    assert_eq!(
        persisted.iter().filter(|u| u.status == "cancelled").count(),
        2
    );
    assert!(!store.start_probe_unit("job", "pending").unwrap());
    assert!(store.active_probe_job_for_scope("scope").unwrap().is_none());
    assert!(
        store
            .create_probe_job(
                "retry",
                "marker_refresh",
                "media",
                Some(1),
                "scope",
                &units[..1]
            )
            .unwrap()
    );
}

fn sample_query(sample: &StoredFingerprintSample) -> store::FingerprintSampleQuery {
    store::FingerprintSampleQuery {
        ledger_id: sample.ledger_id.clone(),
        source_version: sample.source_version.clone(),
        capture_profile_key: sample.capture_profile_key.clone(),
        kind: sample.kind.clone(),
        window_start_ms: sample.window_start_ms,
        window_end_ms: sample.window_end_ms,
        captured_job_id: None,
    }
}

#[test]
fn refresh_cannot_publish_a_snapshot_that_omits_a_deleted_job_member() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let row = row(temp.path());
    store.insert_ledger(&row).unwrap();
    store.put_media_marker(&marker(&row, "old")).unwrap();
    let id = row.id.to_string();
    let units: Vec<_> = [id.as_str(), "deleted-before-snapshot"]
        .iter()
        .map(|id| ProbeJobUnitSpec {
            ledger_id: id,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: true,
            reuse_media_info_cache: true,
        })
        .collect();
    store
        .create_probe_job(
            "job",
            "marker_refresh",
            &row.media_id.to_string(),
            Some(1),
            "scope",
            &units,
        )
        .unwrap();
    for unit in &units {
        store.start_probe_unit("job", unit.ledger_id).unwrap();
        store
            .finish_probe_unit("job", unit.ledger_id, true, None)
            .unwrap();
    }
    let replacement = MarkerResultReplacement {
        media_id: row.media_id,
        season: 1,
        markers: vec![marker(&row, "new")],
        chapter_updates: vec![(id, vec![])],
    };
    assert!(
        store
            .complete_marker_refresh_checked("job", &[replacement])
            .is_err()
    );
    assert_eq!(
        store
            .get_media_marker(row.media_id, Some(1), Some(1))
            .unwrap()
            .unwrap()
            .source,
        "old"
    );
    assert!(store.get_probe_job("job").unwrap().unwrap().is_active());
}

#[test]
fn job_scoped_publication_publishes_rows_the_job_covers() {
    let temp_a = tempfile::tempdir().unwrap();
    let temp_b = tempfile::tempdir().unwrap();
    let store = Store::open(temp_a.path()).unwrap();
    let media_id = MediaId::new();

    let mut row_a = row(temp_a.path());
    row_a.media_id = media_id;
    row_a.season = Some(1);
    row_a.episode = Some(1);
    store.insert_ledger(&row_a).unwrap();

    let mut row_b = row(temp_b.path());
    row_b.media_id = media_id;
    row_b.season = Some(1);
    row_b.episode = Some(2);
    store.insert_ledger(&row_b).unwrap();

    let id_a = row_a.id.to_string();
    let units = [ProbeJobUnitSpec {
        ledger_id: &id_a,
        kind: "tv",
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: true,
        reuse_media_info_cache: true,
    }];
    store
        .create_probe_job(
            "job",
            "marker_refresh",
            &media_id.to_string(),
            Some(1),
            "scope",
            &units,
        )
        .unwrap();
    for unit in &units {
        store.start_probe_unit("job", unit.ledger_id).unwrap();
        store
            .finish_probe_unit("job", unit.ledger_id, true, None)
            .unwrap();
    }

    let expected_chapters_a = vec![library::ChapterMarker {
        start_ms: 0,
        end_ms: 120_000,
        title: Some("Chapter 1".into()),
        marker_type: None,
        synthetic: false,
    }];
    let replacement = MarkerResultReplacement {
        media_id,
        season: 1,
        markers: vec![marker(&row_a, "job_covered")],
        chapter_updates: vec![(id_a.clone(), expected_chapters_a.clone())],
    };

    let result = store.complete_marker_refresh_checked("job", &[replacement]);
    assert!(result.is_ok());
    assert_eq!(
        store.get_cached_chapters(&id_a).unwrap(),
        Some(expected_chapters_a)
    );
    assert_eq!(
        store.get_cached_chapters(&row_b.id.to_string()).unwrap(),
        None
    );
}

fn capture_result(row: &LedgerRow) -> (StoredFingerprintAttempt, StoredFingerprintSample) {
    let mut attempt = StoredFingerprintAttempt {
        attempt_id: format!("capture-{}", row.id),
        job_id: "job".into(),
        ledger_id: row.id.to_string(),
        kind: "intro".into(),
        window_start_ms: 0,
        window_end_ms: 60_000,
        phase: "seed".into(),
        started_at_ms: 1,
        finished_at_ms: None,
        status: "running".into(),
        error_kind: None,
        metrics_json: "{}".into(),
    };
    attempt.status = "succeeded".into();
    attempt.finished_at_ms = Some(2);
    let sample = StoredFingerprintSample {
        sample_id: format!("sample-{}", row.id),
        ledger_id: row.id.to_string(),
        source_version: "v1".into(),
        capture_profile_key: "profile".into(),
        kind: "intro".into(),
        window_start_ms: 0,
        window_end_ms: 60_000,
        pcm_duration_ms: Some(60_000),
        fingerprint: vec![1, 2, 3],
        captured_job_id: "job".into(),
        captured_at_ms: 2,
        metrics_json: "{}".into(),
    };
    (attempt, sample)
}

#[test]
fn deleting_all_media_ledgers_also_removes_their_fingerprint_samples() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let row = row(temp.path());
    store.insert_ledger(&row).unwrap();
    let (attempt, sample) = capture_result(&row);
    assert!(
        store
            .complete_fingerprint_attempt_for_source(&attempt, &sample, &row.path)
            .unwrap()
    );
    assert_eq!(
        store
            .find_covering_fingerprint_samples(&sample_query(&sample))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(store.delete_ledger_for_media(row.media_id).unwrap(), 1);
    assert!(
        store
            .find_covering_fingerprint_samples(&sample_query(&sample))
            .unwrap()
            .is_empty()
    );
    assert!(
        !store
            .complete_fingerprint_attempt_for_source(&attempt, &sample, &row.path)
            .unwrap()
    );
}

#[test]
fn cancelled_refresh_unit_cannot_be_published_as_success() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let row = row(temp.path());
    store.insert_ledger(&row).unwrap();
    store.put_media_marker(&marker(&row, "old")).unwrap();
    let id = row.id.to_string();
    let unit = ProbeJobUnitSpec {
        ledger_id: &id,
        kind: "tv",
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: true,
        reuse_media_info_cache: true,
    };
    store
        .create_probe_job(
            "job",
            "marker_refresh",
            &row.media_id.to_string(),
            Some(1),
            "scope",
            &[unit],
        )
        .unwrap();
    store.cancel_probe_unit("job", &id, "file_deleted").unwrap();
    let replacement = MarkerResultReplacement {
        media_id: row.media_id,
        season: 1,
        markers: vec![marker(&row, "new")],
        chapter_updates: vec![(id.clone(), vec![])],
    };
    assert!(
        store
            .complete_marker_refresh_checked("job", &[replacement])
            .is_err()
    );
    assert!(store.get_probe_job("job").unwrap().unwrap().is_active());
    assert_eq!(
        store
            .get_media_marker(row.media_id, Some(1), Some(1))
            .unwrap()
            .unwrap()
            .source,
        "old"
    );
    assert!(store.get_cached_chapters(&id).unwrap().is_none());
}

#[test]
fn independent_store_handles_cannot_reinsert_samples_while_ledger_is_deleted() {
    use std::sync::{Arc, Barrier};
    let temp = tempfile::tempdir().unwrap();
    let deleting = Store::open(temp.path()).unwrap();
    let capturing = Store::open(temp.path()).unwrap();
    let rows: Vec<_> = (0..32)
        .map(|index| {
            let mut row = row(temp.path());
            row.path = temp
                .path()
                .join(format!("episode-{index}.mkv"))
                .to_string_lossy()
                .into_owned();
            std::fs::write(&row.path, b"fixture").unwrap();
            deleting.insert_ledger(&row).unwrap();
            row
        })
        .collect();
    let gate = Arc::new(Barrier::new(2));
    std::thread::scope(|scope| {
        let capture_gate = gate.clone();
        let rows = &rows;
        scope.spawn(move || {
            for row in rows {
                let (attempt, sample) = capture_result(row);
                capture_gate.wait();
                // A concurrent deletion may reject the write or invalidate a SQLite read
                // snapshot. Either result must leave no sample once deletion commits.
                let _ =
                    capturing.complete_fingerprint_attempt_for_source(&attempt, &sample, &row.path);
                capture_gate.wait();
            }
        });
        for row in rows {
            gate.wait();
            deleting.delete_ledger_path(&row.path).unwrap();
            gate.wait();
        }
    });
    for row in rows {
        let (_, sample) = capture_result(&row);
        assert!(deleting.ledger_by_path(&row.path).unwrap().is_none());
        assert!(
            deleting
                .find_covering_fingerprint_samples(&sample_query(&sample))
                .unwrap()
                .is_empty()
        );
    }
}
