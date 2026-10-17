use api::Store;
use domain::{
    AtomRule, Coverage, FetchMode, Filter, FilterAtom, FilterId, Media, MediaId, MediaKind, SiteId,
    Subscribe, SubscribeId, Torrent, UserId,
};
#[test]
fn ensure_media_keeps_movie_and_tv_with_same_tmdb_id_separate() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let movie = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: Some(1999),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let tv = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "The Matrix (TV)".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let movie_saved = store.ensure_media(movie.clone()).unwrap();
    let tv_saved = store.ensure_media(tv.clone()).unwrap();
    // 相同数字 TMDB id 的 movie/TV 必须得到两个不同的内部 Media。
    assert_ne!(movie_saved.id, tv_saved.id);
    assert_eq!(movie_saved.id, movie.id);
    assert_eq!(tv_saved.id, tv.id);
    // 各自仍能按 (tmdb_id + kind) 找回。
    assert!(
        store
            .get_media_by_alias_kind("tmdb_id", "603", Some(MediaKind::Movie))
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get_media_by_alias_kind("tmdb_id", "603", Some(MediaKind::Tv))
            .unwrap()
            .is_some()
    );
}

#[test]
fn merge_pending_keeps_existing_downloads_across_search_rounds() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let filter = Filter {
        id: FilterId::new(),
        name: "f".into(),
        atoms: vec![FilterAtom {
            priority: 10,
            rule: AtomRule::Resolution("1080p".into()),
            exclude: false,
        }],
        keep_old_versions: false,
};
    store.insert_filter(&filter).unwrap();
    let subscribe = Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: MediaId::new(),
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
    let torrent = |title: &str, enclosure: &str| Torrent {
        id: None,
        site_id: SiteId::new(),
        title: title.into(),
        enclosure: enclosure.into(),
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
    };
    let pending = |t: Torrent, score: i32| {
        (
            score,
            api::store::PendingDownload {
                submitted_at: Some(1),
                torrent: t,
                release_override: None,
                downloader_id: None,
            },
        )
    };

    // 第一轮：候选 A。
    let a = torrent("Movie.2026.1080p", "magnet:a");
    store
        .merge_pending(subscribe.id, &[pending(a.clone(), 80)])
        .unwrap();
    // 第二轮：候选 B 加入，A 重选（分数更新）；旧实现会先 DELETE 全部。
    let b = torrent("Movie.2026.2160p", "magnet:b");
    store
        .merge_pending(
            subscribe.id,
            &[pending(b.clone(), 90), pending(a.clone(), 85)],
        )
        .unwrap();
    let rows = store.load_pending(subscribe.id).unwrap();
    assert_eq!(rows.len(), 2, "两轮后两个候选都必须还在跟踪");
    let scores: Vec<i32> = rows.iter().map(|(s, _)| *s).collect();
    assert!(
        scores.contains(&90) && scores.contains(&85),
        "scores: {scores:?}"
    );
}
