use domain::{Coverage, FetchMode, Media, MediaId, MediaKind, Torrent};
use downloader::{Downloader, DownloaderError};
use std::path::PathBuf;
use subscribe::{
    Added, QualityFact, RunInput, SubscribeFacts, collect_completed_with_destinations,
    collection_destinations::DestinationMapping,
};

struct MockDownloader {
    files: Vec<PathBuf>,
}

impl Downloader for MockDownloader {
    fn add(&self, _torrent: &Torrent) -> Result<(), DownloaderError> {
        Ok(())
    }
    fn remove(&self, _torrent: &Torrent, _delete_files: bool) -> Result<(), DownloaderError> {
        Ok(())
    }
    fn completed_files(&self, _torrent: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        Ok(self.files.clone())
    }
}

fn tv_fixtures() -> (Media, domain::Subscribe, domain::Filter, Torrent) {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Test Show".into(),
        year: Some(2023),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("12345".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let sub = domain::Subscribe {
        id: domain::SubscribeId::new(),
        user_id: domain::UserId::new(),
        media_id: media.id,
        coverage: Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: Some(2),
        },
        fetch_mode: FetchMode::Search,
        filter_id: domain::FilterId::new(),
        wash_cut: false,
        keep_old_versions: false,
        wash_cut_filter_id: None,
        full_season_pack: true,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };
    let filter = domain::Filter::new(sub.filter_id, "test", vec![]);
    let torrent = Torrent {
        id: None,
        site_id: domain::SiteId::new(),
        title: "Test.Show.S01.Complete.1080p".into(),
        enclosure: "magnet:?xt=urn:btih:pack".into(),
        size_bytes: Some(1024),
        seeders: Some(10),
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
    (media, sub, filter, torrent)
}

#[test]
fn season_pack_retry_only_reconciles_mapped_slots_without_forging_facts() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("library");
    std::fs::create_dir_all(&library).unwrap();

    let dest1 = library.join("Test.Show.S01E01.mkv");
    std::fs::write(&dest1, b"e01-video").unwrap();

    let (media, sub, filter, torrent) = tv_fixtures();
    let facts = SubscribeFacts::default();

    let src1 = tmp.path().join("stage/Test.Show.S01E01.mkv");
    std::fs::create_dir_all(src1.parent().unwrap()).unwrap();
    std::fs::write(&src1, b"e01-video").unwrap();

    let existing_mappings = vec![DestinationMapping::with_slots_and_quality(
        src1.display().to_string(),
        dest1.clone(),
        vec![(Some(1), Some(1))],
        None,
    )];

    let dl = MockDownloader { files: vec![src1] };
    let naming = "{title} - S{season:02}E{episode:02}{ext}".to_string();
    let input = RunInput {
        subscribe: &sub,
        media: &media,
        filter: &filter,
        wash_filter: None,
        torrents: vec![],
        facts,
        search_keywords: vec![],
        downloader: &dl,
        library_root: &library,
        transfer_mode: Some(library::TransferMode::Copy),
        scrape: false,
        hooks: None,
        naming: Some(&naming),
        preserve_removed: true,
        overlap: Default::default(),
        absolute: None,
    };

    let added = Added {
        input,
        chosen: vec![filter::ScoredTorrent {
            torrent: torrent.clone(),
            release: release::parse(&torrent.title),
            score: 100,
        }],
        add_errors: vec![],
        rejected: vec![],
        outcome: subscribe::RunOutcome {
            facts: Default::default(),
            ledger: vec![],
            ledger_sources: vec![],
            removed_paths: vec![],
            collection_errors: vec![],
            transferred_enclosures: vec![],
            rejected_enclosures: Vec::new(),
            awaiting_probe_enclosures: Vec::new(),
            completed: false,
            submission_errors: vec![],
            torrents_added: vec![],
        },
        broken: Vec::new(),
    };

    let outcome = collect_completed_with_destinations(added, &library::Ffprobe::default(), &existing_mappings).unwrap();

    // E01 事实恢复，E02 绝不能被凭空赋予相同路径或事实！
    assert_eq!(
        outcome.facts.get(Some(1), Some(1)).and_then(|f| f.path.clone()),
        Some(dest1.display().to_string())
    );
    assert!(outcome.facts.get(Some(1), Some(2)).is_none());
}

#[test]
fn stale_facts_pointing_at_missing_file_are_reconciled_to_new_destination() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("library");
    std::fs::create_dir_all(&library).unwrap();

    let old_dest = library.join("Test.Show.S01E01.720p.mkv");
    let new_dest = library.join("Test.Show.S01E01.1080p.mkv");
    // old 文件已被物理删除，new 文件已在位
    std::fs::write(&new_dest, b"new-video-bytes").unwrap();

    let (media, sub, filter, torrent) = tv_fixtures();
    let mut facts = SubscribeFacts::default();
    // 陈旧失效 fact：指向不存在的 old_dest
    facts.replace(
        Some(1),
        Some(1),
        QualityFact {
            score: 50,
            path: Some(old_dest.display().to_string()),
        },
    );

    let src1 = tmp.path().join("stage/Test.Show.S01E01.1080p.mkv");
    std::fs::create_dir_all(src1.parent().unwrap()).unwrap();
    std::fs::write(&src1, b"new-video-bytes").unwrap();

    let existing_mappings = vec![DestinationMapping::with_slots_and_quality(
        src1.display().to_string(),
        new_dest.clone(),
        vec![(Some(1), Some(1))],
        None,
    )];

    let dl = MockDownloader { files: vec![src1] };
    let naming = "{title} - S{season:02}E{episode:02}{ext}".to_string();
    let input = RunInput {
        subscribe: &sub,
        media: &media,
        filter: &filter,
        wash_filter: None,
        torrents: vec![],
        facts,
        search_keywords: vec![],
        downloader: &dl,
        library_root: &library,
        transfer_mode: Some(library::TransferMode::Copy),
        scrape: false,
        hooks: None,
        naming: Some(&naming),
        preserve_removed: true,
        overlap: Default::default(),
        absolute: None,
    };

    let added = Added {
        input,
        chosen: vec![filter::ScoredTorrent {
            torrent: torrent.clone(),
            release: release::parse(&torrent.title),
            score: 100,
        }],
        add_errors: vec![],
        rejected: vec![],
        outcome: subscribe::RunOutcome {
            facts: Default::default(),
            ledger: vec![],
            ledger_sources: vec![],
            removed_paths: vec![],
            collection_errors: vec![],
            transferred_enclosures: vec![],
            rejected_enclosures: Vec::new(),
            awaiting_probe_enclosures: Vec::new(),
            completed: false,
            submission_errors: vec![],
            torrents_added: vec![],
        },
        broken: Vec::new(),
    };

    let outcome = collect_completed_with_destinations(added, &library::Ffprobe::default(), &existing_mappings).unwrap();

    // 陈旧的 facts 必须被修复指向 new_dest，分值提升为 100
    let fact = outcome.facts.get(Some(1), Some(1)).unwrap();
    assert_eq!(fact.path, Some(new_dest.display().to_string()));
    assert_eq!(fact.score, 100);
}
