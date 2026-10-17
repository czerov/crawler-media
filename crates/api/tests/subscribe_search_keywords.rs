use std::str::FromStr;

use api::{page_is_full_and_irrelevant, search_keywords_for};
use domain::{SiteId, Torrent};
use domain::{Coverage, Media, MediaId, MediaKind};

fn media(aliases: &[&str]) -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "醉梦".into(),
        year: None,
        original_title: None,
        aliases: aliases.iter().map(|alias| (*alias).to_string()).collect(),
        tmdb_id: Some("1".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

#[test]
fn bounded_tv_search_keeps_distinctive_aliases_and_drops_dream() {
    let keywords = search_keywords_for(
        &media(&["Dream", "Chinese Dream", "国庆特别节目"]),
        &Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: Some(3),
        },
    );
    assert_eq!(
        keywords,
        vec![
            "醉梦".to_string(),
            "Chinese Dream".to_string(),
            "国庆特别节目".to_string(),
            "醉梦 S01".to_string(),
        ]
    );
}

#[test]
fn a_fourth_distinctive_alias_is_not_searched() {
    let keywords = search_keywords_for(
        &media(&[
            "Chinese Dream",
            "国庆特别节目",
            "Family Conditions",
            "National Day",
        ]),
        &Coverage::Movie,
    );
    assert_eq!(
        keywords,
        vec![
            "醉梦".to_string(),
            "Chinese Dream".to_string(),
            "国庆特别节目".to_string(),
            "Family Conditions".to_string(),
        ]
    );
    assert!(!keywords.iter().any(|keyword| keyword == "National Day"));
    assert!(!keywords.iter().any(|keyword| keyword.contains("S01")));
}

#[test]
fn ongoing_tv_does_not_add_a_season_keyword() {
    let keywords = search_keywords_for(
        &media(&["Chinese Dream"]),
        &Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: None,
        },
    );
    assert_eq!(keywords, vec!["醉梦".to_string(), "Chinese Dream".to_string()]);
}

fn torrent(title: &str) -> Torrent {
    Torrent {
        site_id: SiteId::from_str("11111111-1111-1111-1111-111111111111").unwrap(),
        title: title.into(),
        enclosure: title.into(),
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
fn full_page_of_the_wrong_season_asks_for_one_more_page() {
    let media = media(&[]);
    let page: Vec<Torrent> = (0..30)
        .map(|index| torrent(&format!("醉梦 S02E{index:02} 1080p")))
        .collect();
    assert!(page_is_full_and_irrelevant(
        &media,
        &Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: Some(12),
        },
        "醉梦 S01",
        &page,
    ));
}

#[test]
fn a_relevant_season_page_does_not_continue() {
    let media = media(&[]);
    let mut page: Vec<Torrent> = (0..29)
        .map(|index| torrent(&format!("Other Show S01E{index:02}")))
        .collect();
    page.push(torrent("醉梦 S01E01 1080p"));
    assert!(!page_is_full_and_irrelevant(
        &media,
        &Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: Some(12),
        },
        "醉梦 S01",
        &page,
    ));
}

#[test]
fn a_short_page_does_not_continue() {
    let media = media(&[]);
    let page = vec![torrent("Other Show S01E01")];
    assert!(!page_is_full_and_irrelevant(
        &media,
        &Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: Some(12),
        },
        "醉梦 S01",
        &page,
    ));
}
