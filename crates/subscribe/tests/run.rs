use std::fs;
use std::path::{Path, PathBuf};

use domain::{
    AtomRule, Coverage, FetchMode, Filter, FilterAtom, FilterId, Media, MediaId, MediaKind, SiteId,
    Subscribe, SubscribeId, Torrent, UserId,
};
use downloader::MemoryDownloader;
use library::{FileQuality, MediaProbe, TransferMode};
use subscribe::{QualityFact, RunInput, SubscribeFacts, run, run_with_probe};

struct FixedProbe;

impl MediaProbe for FixedProbe {
    fn probe(&self, _path: &Path) -> Result<FileQuality, library::LibraryError> {
        Ok(FileQuality {
            resolution: Some("2160p".into()),
            codec: Some("hevc".into()),
            hdr: Some("hdr10".into()),
        })
    }
}

fn movie_filter() -> Filter {
    Filter {
        id: FilterId::new(),
        name: "movie".into(),
        atoms: vec![
            FilterAtom {
                priority: 100,
                rule: AtomRule::Resolution("2160p".into()),
                exclude: false,
            },
            FilterAtom {
                priority: 50,
                rule: AtomRule::Resolution("1080p".into()),
                exclude: false,
            },
            FilterAtom {
                priority: 20,
                rule: AtomRule::Source("BluRay".into()),
                exclude: false,
            },
        ],
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

fn media_tv() -> Media {
    Media {
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
    }
}

fn movie_sub(media: &Media, filter: &Filter, wash_cut: bool) -> Subscribe {
    Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: media.id,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: filter.id,
        wash_cut,
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

fn tv_sub(media: &Media, filter: &Filter, wash_cut: bool, pack: bool) -> Subscribe {
    Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: media.id,
        coverage: Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: Some(3),
        },
        fetch_mode: FetchMode::Search,
        filter_id: filter.id,
        wash_cut,
        keep_old_versions: false,
        wash_cut_filter_id: None,
        full_season_pack: pack,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    }
}

fn torrent(title: &str, enclosure: &str) -> Torrent {
    Torrent {
        site_id: SiteId::new(),
        title: title.into(),
        enclosure: enclosure.into(),
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

fn write_probed(path: &Path, resolution: &str, codec: &str, hdr: &str) {
    fs::write(
        path,
        format!(r#"{{"resolution":"{resolution}","codec":"{codec}","hdr":"{hdr}"}}"#),
    )
    .unwrap();
}

fn map_file(dl: &MemoryDownloader, torrent: &Torrent, src: PathBuf) {
    dl.map_enclosure(&torrent.enclosure, src);
}

#[path = "run/admission.rs"]
mod admission;
#[path = "run/season_pack.rs"]
mod season_pack;
#[path = "run/transfer.rs"]
mod transfer;
#[path = "run/wash_cut.rs"]
mod wash_cut;

fn run_input<'a>(
    subscribe: &'a Subscribe,
    media: &'a Media,
    filter: &'a Filter,
    torrents: Vec<Torrent>,
    facts: SubscribeFacts,
    downloader: &'a MemoryDownloader,
    library_root: &'a Path,
) -> RunInput<'a, MemoryDownloader> {
    RunInput {
        subscribe,
        media,
        filter,
        wash_filter: None,
        torrents,
        search_keywords: vec![],
        facts,
        downloader,
        library_root,
        transfer_mode: Some(TransferMode::Copy),
        scrape: false,
        hooks: None,
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
    }
}
