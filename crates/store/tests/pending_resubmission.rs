use domain::{
    Coverage, FetchMode, Filter, FilterAtom, FilterId, Media, MediaId, MediaKind, SiteId,
    Subscribe, SubscribeId, Torrent, UserId,
};
use store::{PendingDownload, Store};

fn make_test_torrent() -> Torrent {
    Torrent {
        id: None,
        site_id: SiteId::new(),
        title: "Test.Movie.2026.1080p".into(),
        enclosure: "https://pt.example/dl/1".into(),
        size_bytes: Some(100),
        seeders: Some(1),
        free: false,
        hr: false,
        imdb_id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    }
}

fn create_sub(store: &Store) -> (SubscribeId, Torrent) {
    let filter = Filter {
        id: FilterId::new(),
        name: "f".into(),
        atoms: vec![FilterAtom {
            priority: 10,
            rule: domain::AtomRule::Resolution("1080p".into()),
            exclude: false,
        }],
        keep_old_versions: false,
    };
    store.insert_filter(&filter).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Test Movie".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        bangumi_id: None,
        anilist_id: None,
        tvdb_id: None,
    };
    store.insert_media(&media).unwrap();
    let subscribe = Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: media.id,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: filter.id,
        wash_cut: false,
        wash_cut_filter_id: None,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
        keep_old_versions: false,
    };
    store.insert_subscribe(&subscribe).unwrap();
    (subscribe.id, make_test_torrent())
}

fn assert_merge_does_not_bump_timestamp(store: &Store, sub_id: SubscribeId, torrent: &Torrent) {
    store
        .merge_pending(
            sub_id,
            &[(
                60,
                PendingDownload {
                    submitted_at: Some(2000),
                    torrent: torrent.clone(),
                    release_override: None,
                    downloader_id: None,
                },
            )],
        )
        .unwrap();
    let unchanged = store.load_pending(sub_id).unwrap();
    assert_eq!(
        unchanged[0].1.submitted_at,
        Some(1000),
        "普通搜索合并不可延后原有任务的宽限期"
    );
}

#[test]
fn actual_submission_refreshes_submitted_at_timestamp() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (sub_id, torrent) = create_sub(&store);

    // 1. 首次插入旧 pending，提交时间为 1000
    store
        .merge_pending(
            sub_id,
            &[(
                50,
                PendingDownload {
                    submitted_at: Some(1000),
                    torrent: torrent.clone(),
                    release_override: None,
                    downloader_id: None,
                },
            )],
        )
        .unwrap();

    let initial = store.load_pending(sub_id).unwrap();
    assert_eq!(initial[0].1.submitted_at, Some(1000));

    // 2. 模拟普通搜索轮次重复命中候选，不应该重置该任务原有的提交时间戳
    assert_merge_does_not_bump_timestamp(&store, sub_id, &torrent);

    // 3. 用户在界面上明确手动重新投递该种子到下载器：必须更新 submitted_at 为当前时间 3000
    store
        .record_pending_submission(
            sub_id,
            80,
            &PendingDownload {
                submitted_at: Some(3000),
                torrent,
                release_override: None,
                downloader_id: None,
            },
        )
        .unwrap();

    let refreshed = store.load_pending(sub_id).unwrap();
    assert_eq!(
        refreshed[0].1.submitted_at,
        Some(3000),
        "确认重投后必须更新 submitted_at 赋予新的宽限期"
    );
}

#[test]
fn batch_pending_submissions_refreshes_all_timestamps() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (sub_id, torrent) = create_sub(&store);

    store
        .merge_pending(
            sub_id,
            &[(
                50,
                PendingDownload {
                    submitted_at: Some(1000),
                    torrent: torrent.clone(),
                    release_override: None,
                    downloader_id: None,
                },
            )],
        )
        .unwrap();

    // 批量重投新候选（带新时间戳 4000）
    store
        .record_pending_submissions(
            sub_id,
            &[(
                90,
                PendingDownload {
                    submitted_at: Some(4000),
                    torrent: torrent.clone(),
                    release_override: None,
                    downloader_id: None,
                },
            )],
        )
        .unwrap();

    let rows = store.load_pending(sub_id).unwrap();
    assert_eq!(
        rows[0].1.submitted_at,
        Some(4000),
        "批量提交必须为每个重投种子刷新时间戳"
    );
}

/// H2: automatic submissions never revive a rejected row; manual ones do.
#[test]
fn rejected_pending_is_only_revived_by_manual_submission() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (id, torrent) = create_sub(&store);
    let pending = || PendingDownload {
        torrent: torrent.clone(),
        release_override: None,
        downloader_id: None,
        submitted_at: None,
    };
    store.record_pending_submissions(id, &[(10, pending())]).unwrap();
    store.mark_pending_rejected(id, &[torrent.enclosure.clone()]).unwrap();
    assert!(store.load_pending(id).unwrap().is_empty());
    assert!(store.load_rejected_enclosures(id).unwrap().contains(&torrent.enclosure));

    store.record_pending_submissions(id, &[(10, pending())]).unwrap();
    store.merge_pending(id, &[(10, pending())]).unwrap();
    assert!(store.load_pending(id).unwrap().is_empty(), "automatic paths keep it rejected");

    store.record_manual_pending_submissions(id, &[(10, pending())]).unwrap();
    assert_eq!(store.load_pending(id).unwrap().len(), 1, "manual submission revives");
    assert!(store.load_rejected_enclosures(id).unwrap().is_empty());
}

/// Review L-3: rows from a database that predates `last_progress_at` start
/// their stall clock at upgrade time, not at their old `submitted_at`.
#[test]
fn upgrade_starts_stall_clock_for_old_pending_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let (sub_id, torrent) = {
        let store = Store::open(tmp.path()).unwrap();
        let (sub_id, torrent) = create_sub(&store);
        store
            .merge_pending(
                sub_id,
                &[(
                    50,
                    PendingDownload {
                        submitted_at: Some(1000),
                        torrent: torrent.clone(),
                        release_override: None,
                        downloader_id: None,
                    },
                )],
            )
            .unwrap();
        (sub_id, torrent)
    };
    // Simulate the pre-H3 schema.
    let conn = rusqlite::Connection::open(tmp.path().join("subscribe.db")).unwrap();
    conn.execute_batch("ALTER TABLE pending_downloads DROP COLUMN last_progress_at;")
        .unwrap();
    drop(conn);

    let store = Store::open(tmp.path()).unwrap();
    let since = store
        .touch_pending_progress(sub_id, &torrent.enclosure, 0.0, 1_000_000)
        .unwrap()
        .unwrap();
    assert!(
        since > 1000,
        "stall clock restarted at upgrade, got {since}"
    );
}
