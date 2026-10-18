use std::path::{Path, PathBuf};

use domain::{
    AtomRule, Confidence, Coverage, FetchMode, Filter, FilterAtom, FilterId, LedgerId, Media,
    MediaId, MediaKind, QualitySource, Subscribe, SubscribeId, Torrent, UserId,
};
use downloader::{Downloader, DownloaderError};
use filter::ScoredTorrent;
use library::{FileQuality, TransferMode};
use subscribe::{Added, RunInput, RunOutcome, SubscribeFacts, collect_completed};

struct FixedFiles(Vec<PathBuf>);

impl Downloader for FixedFiles {
    fn add(&self, _: &Torrent) -> Result<(), DownloaderError> {
        Ok(())
    }
    fn completed_files(&self, _: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        Ok(self.0.clone())
    }
}

struct Probe;

impl library::MediaProbe for Probe {
    fn probe(&self, _: &Path) -> Result<FileQuality, library::LibraryError> {
        Ok(FileQuality {
            resolution: Some("1080p".into()),
            codec: None,
            hdr: None,
        })
    }
}

#[test]
fn one_failed_file_does_not_discard_successful_files_from_same_torrent() {
    let tmp = tempfile::tempdir().unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "The Expanse".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let filter = Filter {
        id: FilterId::new(),
        name: "default".into(),
        atoms: vec![FilterAtom {
            priority: 1,
            rule: AtomRule::Resolution("1080p".into()),
            exclude: false,
        }],
        keep_old_versions: false,
};
    let subscribe = Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: media.id,
        coverage: Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: Some(2),
        },
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
    let first = tmp.path().join("The.Expanse.S01E01.mkv");
    std::fs::write(&first, b"video").unwrap();
    let missing = tmp.path().join("The.Expanse.S01E02.mkv");
    let downloader = FixedFiles(vec![first, missing]);
    let torrent = Torrent {
        site_id: domain::SiteId::new(),
        title: "The Expanse S01E01-E02 1080p".into(),
        enclosure: "https://pt.example/expanse".into(),
        size_bytes: None,
        seeders: None,
        free: false,
        hr: false,
        imdb_id: None,
        id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    };
    let added = Added {
        input: RunInput {
            subscribe: &subscribe,
            media: &media,
            filter: &filter,
            wash_filter: None,
            torrents: vec![],
            search_keywords: vec![],
            facts: SubscribeFacts::default(),
            downloader: &downloader,
            library_root: &tmp.path().join("library"),
            transfer_mode: Some(TransferMode::Copy),
            scrape: false,
            hooks: None,
            naming: None,
            preserve_removed: false,
            overlap: Default::default(),
            absolute: None,
        },
        chosen: vec![ScoredTorrent {
            release: domain::Release {
                title: torrent.title.clone(),
                year: None,
                season: Some(1),
                episode: Some(1),
                episode_to: Some(2),
                resolution: Some("1080p".into()),
                source: None,
                codec: None,
                hdr: None,
                subtitle_language: None,
                audio_language: None,
                group: None,
                confidence: Confidence::High,
                season_inferred: false,
            },
            torrent,
            score: 80,
        }],
        add_errors: vec![],
        rejected: vec![],
        outcome: RunOutcome {
            facts: SubscribeFacts::default(),
            ledger: vec![],
            ledger_sources: vec![],
            removed_paths: vec![],
            completed: false,
            transferred_enclosures: vec![],
            rejected_enclosures: Vec::new(),
            awaiting_probe_enclosures: Vec::new(),
            collection_errors: vec![],
            submission_errors: vec![],
            torrents_added: vec![],
        },
        broken: Vec::new(),
    };

    let outcome = collect_completed(added, &Probe)
        .expect("partial collection should return persistable results");

    assert_eq!(outcome.ledger.len(), 1);
    assert_eq!(outcome.ledger[0].season, Some(1));
    assert_eq!(outcome.ledger[0].episode, Some(1));
    assert_eq!(outcome.collection_errors.len(), 1);
    assert!(outcome.transferred_enclosures.is_empty());
    assert_eq!(outcome.ledger[0].quality_source, QualitySource::Probe);
    assert_ne!(outcome.ledger[0].id, LedgerId::new());
}
