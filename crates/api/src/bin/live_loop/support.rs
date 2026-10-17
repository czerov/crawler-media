use std::path::PathBuf;

use domain::{
    AtomRule, Coverage, FetchMode, Filter, FilterAtom, FilterId, Media, MediaId, MediaKind, Site,
    SiteId, Subscribe, SubscribeId, Torrent, UserId,
};
use indexer::SearchOutcome;

pub fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("missing {name}"))
}

pub fn site(
    name: &str,
    url: String,
    profile_id: &str,
    cookie: Option<String>,
    api_key: Option<String>,
) -> Site {
    Site {
        id: SiteId::new(),
        name: name.into(),
        url,
        profile_id: profile_id.into(),
        cookie,
        api_key,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: Some(12),
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
    }
}

pub fn print_hits(label: &str, outcome: &SearchOutcome) {
    println!(
        "{label} torrents={} failures={:?}",
        outcome.torrents.len(),
        outcome.failures
    );
    for torrent in outcome.torrents.iter().take(5) {
        println!(
            "  seeders={:?} size={:?} {}",
            torrent.seeders, torrent.size_bytes, torrent.title
        );
    }
}

pub fn choose(mut torrents: Vec<Torrent>) -> Result<Torrent, Box<dyn std::error::Error>> {
    torrents.sort_by_key(|torrent| std::cmp::Reverse(torrent.seeders.unwrap_or(0)));
    torrents
        .into_iter()
        .find(|torrent| {
            torrent.size_bytes.unwrap_or(u64::MAX) < 800_000_000 && torrent.seeders.unwrap_or(0) > 0
        })
        .ok_or_else(|| "no small seeded torrent".into())
}

pub fn subscribe_bundle() -> (Media, Filter, Subscribe) {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "The Long Watch".into(),
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
        name: "live".into(),
        atoms: vec![FilterAtom {
            priority: 80,
            rule: AtomRule::TitleMatch("Long Watch".into()),
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
            episode_to: Some(1),
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
    (media, filter, subscribe)
}
