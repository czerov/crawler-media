use domain::{Coverage, MediaKind};
use library::TransferMode;
use std::fs;
use subscribe::{QualityFact, RunInput, SubscribeFacts};

fn mock_probed_file(path: &std::path::Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"test-video-bytes").unwrap();
}

/// Replacement approval needs a successful probe (S1); report 1080p/h264.
struct Probed1080;

impl library::MediaProbe for Probed1080 {
    fn probe(&self, _: &std::path::Path) -> Result<library::FileQuality, library::LibraryError> {
        Ok(library::FileQuality {
            resolution: Some("1080p".into()),
            codec: Some("h264".into()),
            hdr: None,
        })
    }
}

fn create_tv_wash_cut_sub(
    media_id: domain::MediaId,
    filter_id: domain::FilterId,
) -> domain::Subscribe {
    domain::Subscribe {
        id: domain::SubscribeId::new(),
        user_id: domain::UserId::new(),
        media_id,
        coverage: Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: Some(2),
        },
        fetch_mode: domain::FetchMode::Search,
        filter_id,
        wash_cut: true,
        wash_cut_filter_id: None,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
        keep_old_versions: false,
    }
}

fn create_multi_ep_torrent() -> domain::Torrent {
    domain::Torrent {
        id: None,
        site_id: domain::SiteId::new(),
        title: "Test.Show.S01E01-E02.720p".into(),
        enclosure: "https://pt.example/dl/multi".into(),
        size_bytes: Some(500),
        seeders: Some(5),
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

fn run_wash_cut_test(
    sub: &domain::Subscribe,
    media: &domain::Media,
    filter: &domain::Filter,
    facts: SubscribeFacts,
    multi_torrent: domain::Torrent,
    dl: &downloader::MemoryDownloader,
    lib_root: &std::path::Path,
) -> subscribe::RunOutcome {
    subscribe::run(RunInput {
        subscribe: sub,
        media,
        filter,
        wash_filter: None,
        torrents: vec![multi_torrent.clone()],
        search_keywords: vec![],
        facts,
        downloader: dl,
        library_root: lib_root,
        transfer_mode: Some(TransferMode::Copy),
        scrape: false,
        hooks: None,
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
    })
    .unwrap()
}

fn create_test_media_and_filter() -> (domain::Media, domain::Filter) {
    let media = domain::Media {
        id: domain::MediaId::new(),
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
    let filter = domain::Filter {
        id: domain::FilterId::new(),
        name: "test".into(),
        atoms: vec![domain::FilterAtom {
            priority: 10,
            rule: domain::AtomRule::Resolution("720p".into()),
            exclude: false,
        }],
        keep_old_versions: false,
    };
    (media, filter)
}

#[test]
fn multi_episode_file_does_not_downgrade_existing_high_score_slot() {
    let tmp = tempfile::tempdir().unwrap();
    let (media, filter) = create_test_media_and_filter();
    let sub = create_tv_wash_cut_sub(media.id, filter.id);

    // 1. 本地已有高分 E01 (score = 100)
    let e1_lib = tmp.path().join("lib/Season 1/Test Show - S01E01.mkv");
    mock_probed_file(&e1_lib);
    let mut facts = SubscribeFacts::default();
    facts.upsert(
        Some(1),
        Some(1),
        QualityFact {
            score: 100,
            path: Some(e1_lib.display().to_string()),
        },
    );

    // 2. 模拟包含 E01-E02 的合并文件，评分只有 10 分
    let multi_torrent = create_multi_ep_torrent();
    let multi_src = tmp.path().join("stage/Test.Show.S01E01-E02.mkv");
    mock_probed_file(&multi_src);

    let dl = downloader::MemoryDownloader::new(tmp.path().join("stage"));
    dl.map_enclosure_files(&multi_torrent.enclosure, vec![multi_src]);

    let outcome = run_wash_cut_test(
        &sub,
        &media,
        &filter,
        facts,
        multi_torrent,
        &dl,
        &tmp.path().join("lib"),
    );

    // 3. 关键断言：E01 原有高分文件必须被保留，绝对不可被低分多集文件删除！
    assert!(
        e1_lib.exists(),
        "已有高分 E01 文件绝不可被低分合并文件删除！"
    );
    assert_eq!(
        outcome.facts.get(Some(1), Some(1)).unwrap().score,
        100,
        "已有高分 E01 评分必须保持 100，不可被降级为 10"
    );
}

#[test]
fn wash_cut_does_not_delete_merged_file_still_referenced_by_other_slots() {
    let tmp = tempfile::tempdir().unwrap();
    let (media, mut filter) = create_test_media_and_filter();
    filter.atoms.insert(
        0,
        domain::FilterAtom {
            priority: 90,
            rule: domain::AtomRule::Resolution("1080p".into()),
            exclude: false,
        },
    );
    let sub = create_tv_wash_cut_sub(media.id, filter.id);
    let merged = tmp.path().join("lib/Season 1/Test Show - S01E01-E02.mkv");
    mock_probed_file(&merged);
    let mut facts = SubscribeFacts::default();
    for episode in [1u32, 2] {
        facts.upsert(
            Some(1),
            Some(episode),
            QualityFact {
                score: 10,
                path: Some(merged.display().to_string()),
            },
        );
    }
    // This test exercises a legitimate known-quality upgrade, not approval
    // based on a default/unknown owned quality score.
    facts.set_quality(
        merged.display().to_string(),
        release::parse("Test.Show.S01E01-E02.720p"),
    );
    let e1 = domain::Torrent {
        id: None,
        site_id: domain::SiteId::new(),
        title: "Test.Show.S01E01.1080p".into(),
        enclosure: "https://pt.example/dl/e1".into(),
        size_bytes: Some(400),
        seeders: Some(8),
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
    let e1_src = tmp.path().join("stage/Test.Show.S01E01.1080p.mkv");
    mock_probed_file(&e1_src);
    let dl = downloader::MemoryDownloader::new(tmp.path().join("stage"));
    dl.map_enclosure_files(&e1.enclosure, vec![e1_src]);
    let outcome = subscribe::run_with_probe(RunInput {
        subscribe: &sub,
        media: &media,
        filter: &filter,
        wash_filter: None,
        torrents: vec![e1],
        search_keywords: vec![],
        facts,
        downloader: &dl,
        library_root: &tmp.path().join("lib"),
        transfer_mode: Some(TransferMode::Copy),
        scrape: false,
        hooks: None,
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
    }, &Probed1080)
    .unwrap();
    assert!(
        merged.exists(),
        "E02 仍引用合并文件时绝不能因 E01 洗版而删除"
    );
    assert_eq!(
        outcome.facts.get(Some(1), Some(2)).unwrap().path.as_deref(),
        Some(merged.display().to_string().as_str())
    );
    assert_eq!(
        outcome.facts.get(Some(1), Some(1)).unwrap().score,
        90,
        "E01 应以更高分单集替换，但不能带走 E02 仍在用的合并文件"
    );
}
