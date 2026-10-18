use domain::{Media, MediaId, MediaKind, Torrent};
use downloader::{Downloader, DownloaderError};
use std::path::PathBuf;
use subscribe::{Added, RunInput, SubscribeFacts, collect_completed_with_destinations};

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

include!("sidecar_retry/cases.rs");
include!("sidecar_retry/matching.rs");

fn make_movie_fixtures() -> (Media, domain::Subscribe, domain::Filter, Torrent) {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Movie".into(),
        year: Some(1999),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        bangumi_id: None,
        anilist_id: None,
        tvdb_id: None,
    };
    let sub = domain::Subscribe {
        id: domain::SubscribeId::new(),
        user_id: domain::UserId::new(),
        media_id: media.id,
        coverage: domain::Coverage::Movie,
        fetch_mode: domain::FetchMode::Search,
        filter_id: domain::FilterId::new(),
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
    let filter = domain::Filter {
        id: sub.filter_id,
        name: "test".into(),
        atoms: vec![],
        keep_old_versions: false,
    };
    let torrent = Torrent {
        id: None,
        site_id: domain::SiteId::new(),
        title: "Movie.1999.1080p".into(),
        enclosure: "magnet:?xt=urn:btih:movie".into(),
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

fn make_added<'a>(
    input: RunInput<'a, MockDownloader>,
    torrent: Torrent,
    release: domain::Release,
) -> Added<'a, MockDownloader> {
    Added {
        input,
        chosen: vec![filter::ScoredTorrent {
            torrent,
            release,
            score: 100,
        }],
        add_errors: vec![],
        rejected: vec![],
        outcome: subscribe::RunOutcome {
            facts: Default::default(),
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
    }
}
