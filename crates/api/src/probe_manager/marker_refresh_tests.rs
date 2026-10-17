#[test]
fn season_refresh_keeps_old_markers_until_success_then_clears_empty_result() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let media_id = MediaId::new();
    store
        .lock()
        .insert_media(&domain::Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "万神殿".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
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
        path: tmp
            .path()
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
    let rows = [make_row(1), make_row(2)];
    for row in &rows {
        std::fs::write(&row.path, b"dummy").unwrap();
        store.lock().insert_ledger(row).unwrap();
    }
    let cached = seed_old_refresh_result(&store.lock(), media_id, &rows[0].id.to_string());

    let manager = ProbeManager::new(store.clone());
    let units = rows
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
    assert_eq!(manager.enqueue_forced_item_refresh(units).unwrap(), 2);
    let mut rx = manager.metadata_rx.try_lock().unwrap();
    let first = rx.try_recv().unwrap();
    let second = rx.try_recv().unwrap();
    manager.finish(&first, true);

    let current = store
        .lock()
        .get_media_marker(media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(current.source, "old-result");
    assert_eq!(
        store
            .lock()
            .get_cached_chapters(&rows[0].id.to_string())
            .unwrap()
            .unwrap(),
        cached
    );

    manager.finish(&second, true);
    assert!(
        store
            .lock()
            .get_media_marker(media_id, Some(1), Some(1))
            .unwrap()
            .is_none()
    );
    let chapters = store
        .lock()
        .get_cached_chapters(&rows[0].id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(chapters.len(), 2);
    assert!(chapters.iter().all(|chapter| chapter.marker_type.is_none()));
    assert_eq!(
        store
            .lock()
            .latest_marker_refresh_for_season(&media_id.to_string(), 1)
            .unwrap()
            .unwrap()
            .status,
        "succeeded"
    );
}

fn seed_old_refresh_result(
    store: &Store,
    media_id: MediaId,
    ledger_id: &str,
) -> Vec<marker::ChapterMarker> {
    store
        .put_media_marker(&crate::store::StoredMediaMarker {
            media_id,
            season: 1,
            episode: 1,
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
