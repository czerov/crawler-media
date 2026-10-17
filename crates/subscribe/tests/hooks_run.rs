use std::fs;

use domain::{
    AtomRule, Coverage, FetchMode, Filter, FilterAtom, FilterId, Media, MediaId, MediaKind, SiteId,
    Subscribe, SubscribeId, Torrent, UserId,
};
use downloader::MemoryDownloader;
use hooks::{Bus, Hook, PluginError, Step};
use library::TransferMode;
use subscribe::{RunInput, SubscribeFacts, run};

#[test]
fn veto_hook_prevents_add_to_downloader() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("The.Matrix.1999.2160p.BluRay.mkv");
    fs::write(&src, b"video").unwrap();
    let downloader = MemoryDownloader::new(tmp.path().join("stage"));
    downloader.map_enclosure("https://pt.example/a", src);
    let filter = Filter {
        id: FilterId::new(),
        name: "movie".into(),
        atoms: vec![FilterAtom {
            priority: 100,
            rule: AtomRule::Resolution("2160p".into()),
            exclude: false,
        }],
        keep_old_versions: false,
};
    let media = Media {
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
    };
    let subscribe = Subscribe {
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
    };
    let bus = Bus::new();
    bus.register(Hook::new(Step::AddDownload, |_| {
        Err(PluginError::Veto("blocked".into()))
    }));
    let result = run(RunInput {
        subscribe: &subscribe,
        media: &media,
        filter: &filter,
        wash_filter: None,
        torrents: vec![Torrent {
            site_id: SiteId::new(),
            title: "The.Matrix.1999.2160p.BluRay".into(),
            enclosure: "https://pt.example/a".into(),
            size_bytes: Some(1),
            seeders: Some(1),
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
        }],
        search_keywords: vec![],
        facts: SubscribeFacts::default(),
        downloader: &downloader,
        library_root: &tmp.path().join("lib"),
        transfer_mode: Some(TransferMode::Copy),
        scrape: false,
        hooks: Some(&bus),
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
    });
    assert!(result.is_err());
    assert!(downloader.added().is_empty());
}
