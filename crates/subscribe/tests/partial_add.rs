use std::path::PathBuf;

use domain::{
    AtomRule, Coverage, FetchMode, Filter, FilterAtom, FilterId, Media, MediaId, MediaKind,
    Subscribe, SubscribeId, Torrent, UserId,
};
use downloader::{Downloader, DownloaderError};
use subscribe::{RunInput, SubscribeFacts, admit_and_add};

struct FailsOne;

impl Downloader for FailsOne {
    fn add(&self, torrent: &Torrent) -> Result<(), DownloaderError> {
        if torrent.enclosure.ends_with("/bad") {
            Err(DownloaderError::Message("injected failure".into()))
        } else {
            Ok(())
        }
    }
    fn completed_files(&self, _: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        Ok(vec![])
    }
}

fn torrent(title: &str, suffix: &str) -> Torrent {
    Torrent {
        site_id: domain::SiteId::new(),
        title: title.into(),
        enclosure: format!("https://pt.example/{suffix}"),
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
    }
}

#[test]
fn successful_download_additions_are_returned_when_another_add_fails() {
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
        name: "1080p".into(),
        atoms: vec![FilterAtom {
            priority: 10,
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
    let second_release = release::parse("The.Expanse.2020.S01E02.1080p.WEB-DL");
    assert_eq!(second_release.episode, Some(2));
    assert!(subscribe::candidate_matches_subscribe(
        &subscribe,
        &media,
        &second_release
    ));
    let downloader = FailsOne;
    let added = admit_and_add(RunInput {
        subscribe: &subscribe,
        media: &media,
        filter: &filter,
        wash_filter: None,
        torrents: vec![
            torrent("The.Expanse.2020.S01E01.1080p.WEB-DL", "good"),
            torrent("The.Expanse.2020.S01E02.1080p.WEB-DL", "bad"),
        ],
        search_keywords: vec![],
        facts: SubscribeFacts::default(),
        downloader: &downloader,
        library_root: std::path::Path::new("/tmp/library"),
        transfer_mode: None,
        scrape: false,
        hooks: None,
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
    })
    .unwrap();

    assert_eq!(added.chosen.len(), 1);
    assert_eq!(added.chosen[0].torrent.enclosure, "https://pt.example/good");
    assert_eq!(added.add_errors.len(), 1);
}
