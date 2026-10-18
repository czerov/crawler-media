use std::fs;
use std::path::Path;

use domain::{
    AtomRule, Coverage, FetchMode, Filter, FilterAtom, FilterId, Media, MediaId, MediaKind, SiteId,
    Subscribe, SubscribeId, Torrent, UserId,
};
use downloader::MemoryDownloader;
use library::TransferMode;
use subscribe::{RunInput, SubscribeFacts, run};

fn movie_filter() -> Filter {
    Filter {
        id: FilterId::new(),
        name: "movie".into(),
        atoms: vec![FilterAtom {
            priority: 100,
            rule: AtomRule::Resolution("2160p".into()),
            exclude: false,
        }],
        keep_old_versions: false,
}
}

fn media_movie() -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn movie_sub(media: &Media, filter: &Filter) -> Subscribe {
    Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: media.id,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: filter.id,
        wash_cut: false,
        keep_old_versions: false,
        wash_cut_filter_id: None,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    }
}

fn torrent() -> Torrent {
    Torrent {
        site_id: SiteId::new(),
        title: "The.Matrix.1999.2160p.BluRay.x265-GROUP".into(),
        enclosure: "https://pt.example/dl/subs".into(),
        size_bytes: Some(1_000),
        seeders: Some(5),
        free: true,
        hr: false,
        imdb_id: None,
        id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    }
}

#[test]
fn collect_transfers_subtitle_sidecar_beside_video() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter);
    let t = torrent();
    let video = tmp.path().join("The.Matrix.1999.2160p.mkv");
    let srt = tmp.path().join("The.Matrix.1999.2160p.srt");
    fs::write(
        &video,
        br#"{"resolution":"2160p","codec":"hevc","hdr":"hdr10"}"#,
    )
    .unwrap();
    fs::write(&srt, b"1\n00:00:01,000 --> 00:00:02,000\nWake up").unwrap();
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    dl.map_enclosure_files(&t.enclosure, vec![video, srt]);
    let library_root = tmp.path().join("lib");

    let outcome = run(RunInput {
        subscribe: &sub,
        media: &media,
        filter: &filter,
        wash_filter: None,
        torrents: vec![t],
        search_keywords: vec![],
        facts: SubscribeFacts::default(),
        downloader: &dl,
        library_root: &library_root,
        transfer_mode: Some(TransferMode::Copy),
        scrape: false,
        hooks: None,
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
    })
    .unwrap();

    assert_eq!(outcome.ledger.len(), 1);
    let video_dest = Path::new(&outcome.ledger[0].path);
    assert!(video_dest.is_file());
    assert_eq!(video_dest.extension().and_then(|e| e.to_str()), Some("mkv"));
    let srt_dest = video_dest.with_extension("srt");
    assert!(srt_dest.is_file(), "missing sidecar {}", srt_dest.display());
    assert!(fs::read_to_string(srt_dest).unwrap().contains("Wake up"));
}

#[test]
fn subtitle_only_torrent_does_not_enter_library() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter);
    let t = torrent();
    let srt = tmp.path().join("The.Matrix.1999.2160p.srt");
    fs::write(&srt, b"subs").unwrap();
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    dl.map_enclosure_files(&t.enclosure, vec![srt]);
    let library_root = tmp.path().join("lib");

    let outcome = run(RunInput {
        subscribe: &sub,
        media: &media,
        filter: &filter,
        wash_filter: None,
        torrents: vec![t],
        search_keywords: vec![],
        facts: SubscribeFacts::default(),
        downloader: &dl,
        library_root: &library_root,
        transfer_mode: Some(TransferMode::Copy),
        scrape: false,
        hooks: None,
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
    })
    .unwrap();

    assert!(outcome.ledger.is_empty());
    assert!(
        library_root
            .exists()
            .then(|| fs::read_dir(&library_root).unwrap().next().is_none())
            .unwrap_or(true)
    );
}

fn create_test_tv_sub(media_id: MediaId, filter_id: FilterId) -> Subscribe {
    Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id,
        coverage: Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: Some(2),
        },
        fetch_mode: FetchMode::Search,
        filter_id,
        wash_cut: false,
        keep_old_versions: false,
        wash_cut_filter_id: None,
        full_season_pack: true,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    }
}

fn create_test_pack_torrent() -> Torrent {
    Torrent {
        site_id: SiteId::new(),
        title: "Test.Show.S01E01-E02.1080p.BluRay".into(),
        enclosure: "https://pt.example/dl/pack".into(),
        size_bytes: Some(2_000),
        seeders: Some(5),
        free: true,
        hr: false,
        imdb_id: None,
        id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    }
}

fn create_test_pack_files(
    tmp: &tempfile::TempDir,
) -> (
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let v1 = tmp.path().join("Test.Show.S01E01.1080p.mkv");
    let v2 = tmp.path().join("Test.Show.S01E02.1080p.mkv");
    let s1 = tmp.path().join("Test.Show.S01E01.1080p.zh.srt");
    let s2 = tmp.path().join("Test.Show.S01E02.1080p.zh.srt");
    fs::write(&v1, br#"{"resolution":"1080p","codec":"h264","hdr":null}"#).unwrap();
    fs::write(&v2, br#"{"resolution":"1080p","codec":"h264","hdr":null}"#).unwrap();
    fs::write(&s1, b"E01 subtitle content").unwrap();
    fs::write(&s2, b"E02 subtitle content").unwrap();
    (v1, v2, s1, s2)
}

#[test]
fn multi_episode_subtitles_match_respective_episodes_without_overwriting() {
    let tmp = tempfile::tempdir().unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Test Show".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        bangumi_id: None,
        anilist_id: None,
        tvdb_id: None,
    };
    let filter = Filter {
        id: FilterId::new(),
        name: "tv".into(),
        atoms: vec![FilterAtom {
            priority: 100,
            rule: AtomRule::Resolution("1080p".into()),
            exclude: false,
        }],
        keep_old_versions: false,
};
    let sub = create_test_tv_sub(media.id, filter.id);
    let t = create_test_pack_torrent();
    let (v1, v2, s1, s2) = create_test_pack_files(&tmp);

    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    dl.map_enclosure_files(&t.enclosure, vec![v1, v2, s1, s2]);

    let outcome = run(RunInput {
        subscribe: &sub,
        media: &media,
        filter: &filter,
        wash_filter: None,
        torrents: vec![t],
        search_keywords: vec![],
        facts: SubscribeFacts::default(),
        downloader: &dl,
        library_root: &tmp.path().join("lib"),
        transfer_mode: Some(TransferMode::Copy),
        scrape: false,
        hooks: None,
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
    })
    .unwrap();

    assert_eq!(outcome.ledger.len(), 2, "两集都应成功入库");
    let p1 = Path::new(
        &outcome
            .ledger
            .iter()
            .find(|r| r.episode == Some(1))
            .unwrap()
            .path,
    );
    let p2 = Path::new(
        &outcome
            .ledger
            .iter()
            .find(|r| r.episode == Some(2))
            .unwrap()
            .path,
    );

    let srt1 = p1.with_extension("zh.srt");
    assert!(
        srt1.is_file(),
        "E01 必须生成自己的字幕文件: {}",
        srt1.display()
    );
    assert_eq!(fs::read_to_string(&srt1).unwrap(), "E01 subtitle content");

    let srt2 = p2.with_extension("zh.srt");
    assert!(
        srt2.is_file(),
        "E02 必须生成自己的字幕文件: {}",
        srt2.display()
    );
    assert_eq!(fs::read_to_string(&srt2).unwrap(), "E02 subtitle content");
}
