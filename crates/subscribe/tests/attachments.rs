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

/// 下载目录里的 NFO / 图片 / 文本附件绝不能进入视频流程：
/// 不 probe、不进 ledger、不触发洗版删除。
#[test]
fn attachments_are_not_treated_as_video() {
    let tmp = tempfile::tempdir().unwrap();
    let filter = movie_filter();
    let media = media_movie();
    let sub = movie_sub(&media, &filter);
    let t = Torrent {
        id: None,
        site_id: SiteId::new(),
        title: "The.Matrix.1999.2160p.BluRay.x265.mkv".into(),
        enclosure: "magnet:?xt=urn:btih:attachments-test".into(),
        size_bytes: Some(1_000),
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
    let nfo = tmp.path().join("The.Matrix.1999.2160p.nfo");
    let poster = tmp.path().join("poster.jpg");
    let readme = tmp.path().join("README.txt");
    for p in [&nfo, &poster, &readme] {
        fs::write(p, b"not video").unwrap();
    }
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    dl.map_enclosure_files(&t.enclosure, vec![nfo, poster, readme]);
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

    // 没有视频文件 → 什么都不入库，ledger 为空、无事实。
    assert!(outcome.ledger.is_empty());
    assert!(outcome.facts.movie().is_none());
    assert!(
        library_root
            .exists()
            .then(|| fs::read_dir(&library_root).unwrap().next().is_none())
            .unwrap_or(true)
    );
}
