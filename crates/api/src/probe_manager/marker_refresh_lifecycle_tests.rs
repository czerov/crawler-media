fn seed_episode_old_refresh_result(
    store: &Store,
    media_id: MediaId,
    ledger_id: &str,
    episode: u32,
) -> Vec<marker::ChapterMarker> {
    store
        .put_media_marker(&crate::store::StoredMediaMarker {
            media_id,
            season: 1,
            episode,
            intro_start_ms: Some(111_000),
            intro_end_ms: Some(222_000),
            outro_start_ms: Some(1_000_000),
            outro_end_ms: Some(1_050_000),
            source: "old-result".into(),
            locked: false,
            updated_at: 0,
        })
        .unwrap();
    let cached = vec![
        marker::ChapterMarker {
            start_ms: 0,
            end_ms: 300_000,
            title: Some("第一章".into()),
            marker_type: None,
            synthetic: false,
        },
        marker::ChapterMarker {
            start_ms: 300_000,
            end_ms: 600_000,
            title: Some("第二章".into()),
            marker_type: None,
            synthetic: false,
        },
        marker::ChapterMarker {
            start_ms: 111_000,
            end_ms: 222_000,
            title: Some("片头".into()),
            marker_type: Some(marker::MarkerType::IntroStart),
            synthetic: false,
        },
        marker::ChapterMarker {
            start_ms: 1_000_000,
            end_ms: 1_050_000,
            title: Some("片尾".into()),
            marker_type: Some(marker::MarkerType::CreditsStart),
            synthetic: false,
        },
    ];
    store.put_cached_chapters(ledger_id, &cached).unwrap();

    cached
}

struct RefreshHarness {
    _tmp: tempfile::TempDir,
    store: Arc<Mutex<Store>>,
    registry: Arc<crate::library_lifecycle::LibraryWorkRegistry>,
    manager: Arc<ProbeManager>,
    media_id: MediaId,
    library_id: String,
    rows: Vec<LedgerRow>,
    expected_chapters: Vec<Vec<marker::ChapterMarker>>,
    scope: String,
}

impl RefreshHarness {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let lib_dir = tmp.path().join("tv_lib");
        std::fs::create_dir_all(&lib_dir).unwrap();
        let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
        let media_id = MediaId::new();
        let library = store
            .lock()
            .create_library(
                MediaKind::Tv,
                "TV",
                &[lib_dir.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        store
            .lock()
            .insert_media(&domain::Media {
                id: media_id,
                kind: MediaKind::Tv,
                title: "万神殿".into(),
                year: None,
                original_title: None,
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let make_row = |episode| LedgerRow {
            id: LedgerId::new(),
            media_id,
            path: lib_dir
                .join(format!("Pantheon.S01E{episode:02}.mkv"))
                .to_string_lossy()
                .into_owned(),
            season: Some(1),
            episode: Some(episode),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        };
        let rows = vec![make_row(1), make_row(2)];
        for row in &rows {
            std::fs::write(&row.path, b"dummy").unwrap();
            store.lock().insert_ledger(row).unwrap();
        }
        let expected_chapters = {
            let store = store.lock();
            vec![
                seed_episode_old_refresh_result(&store, media_id, &rows[0].id.to_string(), 1),
                seed_episode_old_refresh_result(&store, media_id, &rows[1].id.to_string(), 2),
            ]
        };

        let registry = Arc::new(crate::library_lifecycle::LibraryWorkRegistry::default());
        let manager = Arc::new(ProbeManager::new(store.clone()));
        manager.set_library_registry(registry.clone());
        let scope = format!("marker-refresh:{media_id}:all");

        Self {
            _tmp: tmp,
            store,
            registry,
            manager,
            media_id,
            library_id: library.id,
            rows,
            expected_chapters,
            scope,
        }
    }

    fn make_units(&self) -> Vec<ProbeUnit> {
        self.rows
            .iter()
            .map(|row| ProbeUnit {
                row: row.clone(),
                kind: MediaKind::Tv,
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: true,
                reuse_media_info_cache: true,
                marker_refresh_id: None,
                job_id: None,
            })
            .collect()
    }

    fn assert_old_markers_intact(&self) {
        let store = self.store.lock();
        for (idx, row) in self.rows.iter().enumerate() {
            let marker = store
                .get_media_marker(self.media_id, row.season, row.episode)
                .unwrap()
                .expect("旧标记事实必须保持可见");
            assert_eq!(marker.source, "old-result");
            assert_eq!(marker.intro_start_ms, Some(111_000));
            assert_eq!(marker.intro_end_ms, Some(222_000));
            assert_eq!(marker.outro_start_ms, Some(1_000_000));
            assert_eq!(marker.outro_end_ms, Some(1_050_000));
            let chapters = store
                .get_cached_chapters(&row.id.to_string())
                .unwrap()
                .expect("旧章节缓存必须存在");
            assert_eq!(chapters, self.expected_chapters[idx]);
            assert!(chapters.iter().any(|c| c.marker_type.is_some()));
        }
    }
}




#[test]
fn window1_queued_stage_cancellation_releases_scope() {
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
        .unwrap();

    harness
        .manager
        .cancel_ledger_work(&[harness.rows[0].id.to_string()], "user_abort")
        .unwrap();

    let job = harness
        .store
        .lock()
        .get_probe_job(&active_job.id)
        .unwrap()
        .unwrap();
    assert_eq!(job.status, "cancelled");
    assert!(
        harness
            .store
            .lock()
            .active_probe_job_for_scope(&harness.scope)
            .unwrap()
            .is_none()
    );
    harness.assert_old_markers_intact();

    let first = harness
        .manager
        .metadata_rx
        .try_lock()
        .unwrap()
        .try_recv()
        .unwrap();
    assert!(!worker::source_ready(&harness.manager, &first));
    assert!(!harness.manager.seen.lock().contains(&first.row.id.to_string()));
}


#[test]
fn window3_pre_publication_freeze_prevents_late_result() {
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
    let second = harness
        .manager
        .metadata_rx
        .try_lock()
        .unwrap()
        .try_recv()
        .unwrap();

    harness.manager.finish(&first, true);
    assert_eq!(
        harness
            .store
            .lock()
            .get_probe_job(&job_id)
            .unwrap()
            .unwrap()
            .completed,
        1
    );

    harness
        .store
        .lock()
        .begin_library_removal(&harness.library_id)
        .unwrap();
    harness.registry.cancel(&harness.library_id);

    harness.manager.finish(&second, true);

    let job = harness.store.lock().get_probe_job(&job_id).unwrap().unwrap();
    assert_ne!(job.status, "succeeded");
    assert_eq!(job.status, "cancelled");
    assert_eq!(job.error.as_deref(), Some("file_deleted"));
    harness.assert_old_markers_intact();
    assert!(
        harness
            .store
            .lock()
            .active_probe_job_for_scope(&harness.scope)
            .unwrap()
            .is_none()
    );
    assert!(!harness.registry.has_active_work());
}

#[test]
fn window3_pre_publication_file_deleted_prevents_late_result() {
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
    let second = harness
        .manager
        .metadata_rx
        .try_lock()
        .unwrap()
        .try_recv()
        .unwrap();

    harness.manager.finish(&first, true);

    std::fs::remove_file(&harness.rows[0].path).unwrap();

    harness.manager.finish(&second, true);

    let job = harness.store.lock().get_probe_job(&job_id).unwrap().unwrap();
    assert_ne!(job.status, "succeeded");
    assert_eq!(job.status, "cancelled");
    assert_eq!(job.error.as_deref(), Some("file_deleted"));
    harness.assert_old_markers_intact();
    assert!(
        harness
            .store
            .lock()
            .active_probe_job_for_scope(&harness.scope)
            .unwrap()
            .is_none()
    );
}

#[test]
fn window4_recovery_interrupted_with_frozen_library_cancels_cleanly() {
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

    harness
        .store
        .lock()
        .begin_library_removal(&harness.library_id)
        .unwrap();

    let restarted = ProbeManager::new(harness.store.clone());
    restarted.recover_pending_jobs();

    let job = harness.store.lock().get_probe_job(&job_id).unwrap().unwrap();
    assert_eq!(job.status, "cancelled");
    assert_eq!(job.error.as_deref(), Some("Library removed"));
    assert!(!restarted.is_queued(&harness.rows[0].id.to_string()));
    assert!(
        harness
            .store
            .lock()
            .active_probe_job_for_scope(&harness.scope)
            .unwrap()
            .is_none()
    );
    harness.assert_old_markers_intact();

    restarted.recover_pending_jobs();
    assert_eq!(
        harness
            .store
            .lock()
            .get_probe_job(&job_id)
            .unwrap()
            .unwrap()
            .status,
        "cancelled"
    );
}

fn assert_recovery_cancels_after_member_deletion(deleted_index: usize) {
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

    harness
        .store
        .lock()
        .finish_probe_unit_with_status(
            &job_id,
            &harness.rows[0].id.to_string(),
            "succeeded",
            None,
            None,
        )
        .unwrap();
    harness
        .store
        .lock()
        .finish_probe_unit_with_status(
            &job_id,
            &harness.rows[1].id.to_string(),
            "succeeded",
            None,
            None,
        )
        .unwrap();

    std::fs::remove_file(&harness.rows[deleted_index].path).unwrap();

    let restarted = ProbeManager::new(harness.store.clone());
    restarted.recover_pending_jobs();

    let job = harness.store.lock().get_probe_job(&job_id).unwrap().unwrap();
    assert_eq!(job.status, "cancelled");
    assert_eq!(job.error.as_deref(), Some("file_deleted"));
    assert!(
        harness
            .store
            .lock()
            .active_probe_job_for_scope(&harness.scope)
            .unwrap()
            .is_none()
    );
    harness.assert_old_markers_intact();
}

#[test]
fn window4_recovery_completed_units_with_deleted_source_cancels_job() {
    for deleted_index in [0, 1] {
        assert_recovery_cancels_after_member_deletion(deleted_index);
    }
}


// Season split across two libraries, refresh requested from library A only.
#[test]
fn window6_cross_library_season_refresh_publishes_only_job_members() {
    let harness = RefreshHarness::new();
    let lib_b_dir = harness._tmp.path().join("tv_lib_b");
    std::fs::create_dir_all(&lib_b_dir).unwrap();
    {
        let store = harness.store.lock();
        store
            .create_library(
                MediaKind::Tv,
                "TV B",
                &[lib_b_dir.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
    }
    let row_b = LedgerRow {
        id: LedgerId::new(),
        media_id: harness.media_id,
        path: lib_b_dir
            .join("Pantheon.S01E03.mkv")
            .to_string_lossy()
            .into_owned(),
        season: Some(1),
        episode: Some(3),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    std::fs::write(&row_b.path, b"dummy").unwrap();
    let expected_chapters_b = {
        let store = harness.store.lock();
        store.insert_ledger(&row_b).unwrap();
        seed_episode_old_refresh_result(
            &store,
            harness.media_id,
            &row_b.id.to_string(),
            3,
        )
    };

    let units: Vec<ProbeUnit> = harness
        .rows
        .iter()
        .map(|row| ProbeUnit {
            row: row.clone(),
            kind: MediaKind::Tv,
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: true,
            reuse_media_info_cache: true,
            marker_refresh_id: None,
            job_id: None,
        })
        .collect();
    assert_eq!(harness.manager.enqueue_marker_refresh(units), 2);
    let job_id = {
        let store = harness.store.lock();
        store
            .active_probe_unit_for_ledger(&harness.rows[0].id.to_string())
            .unwrap()
            .unwrap()
            .job_id
    };

    let first = harness
        .manager
        .metadata_rx
        .try_lock()
        .unwrap()
        .try_recv()
        .unwrap();
    let second = harness
        .manager
        .metadata_rx
        .try_lock()
        .unwrap()
        .try_recv()
        .unwrap();
    harness.manager.finish(&first, true);
    harness.manager.finish(&second, true);

    let (job, chapters_b, chapters_a0, chapters_a1, marker_b, active_job) = {
        let store = harness.store.lock();
        (
            store.get_probe_job(&job_id).unwrap().unwrap(),
            store.get_cached_chapters(&row_b.id.to_string()).unwrap(),
            store
                .get_cached_chapters(&harness.rows[0].id.to_string())
                .unwrap(),
            store
                .get_cached_chapters(&harness.rows[1].id.to_string())
                .unwrap(),
            store
                .get_media_marker(harness.media_id, row_b.season, row_b.episode)
                .unwrap(),
            store.active_probe_job_for_scope(&harness.scope).unwrap(),
        )
    };
    assert_eq!(job.status, "succeeded");
    assert_eq!(chapters_b, Some(expected_chapters_b));
    assert!(chapters_a0.is_some());
    assert!(chapters_a1.is_some());
    let chapters_a0 = chapters_a0.unwrap();
    let chapters_a1 = chapters_a1.unwrap();
    assert_ne!(chapters_a0, harness.expected_chapters[0]);
    assert_ne!(chapters_a1, harness.expected_chapters[1]);
    assert!(!chapters_a0.iter().any(|c| c.marker_type.is_some()));
    assert!(!chapters_a1.iter().any(|c| c.marker_type.is_some()));
    let marker_b = marker_b.expect("library B marker must still exist");
    assert_eq!(marker_b.source, "old-result");
    assert_eq!(marker_b.intro_start_ms, Some(111_000));
    assert_eq!(marker_b.intro_end_ms, Some(222_000));
    assert_eq!(marker_b.outro_start_ms, Some(1_000_000));
    assert_eq!(marker_b.outro_end_ms, Some(1_050_000));
    assert!(active_job.is_none());
}
