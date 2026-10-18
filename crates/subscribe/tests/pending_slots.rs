//! H4: one placeholder rule for in-flight pending releases.

use domain::Coverage;
use subscribe::{QualityFact, SubscribeFacts, facts_with_pending, pending_slots};

fn media() -> domain::Media {
    domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Tv,
        title: "Test Show".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        bangumi_id: None,
        anilist_id: None,
        tvdb_id: None,
    }
}

fn sub(media: &domain::Media, full_season_pack: bool) -> domain::Subscribe {
    domain::Subscribe {
        id: domain::SubscribeId::new(),
        user_id: domain::UserId::new(),
        media_id: media.id,
        coverage: Coverage::Tv { season: 1, episode_from: 1, episode_to: Some(3) },
        fetch_mode: domain::FetchMode::Search,
        filter_id: domain::FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        full_season_pack,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
        keep_old_versions: false,
    }
}

#[test]
fn pure_season_pack_covers_the_whole_window_in_both_modes() {
    let media = media();
    let pack = release::parse("Test.Show.S01.1080p.WEB-DL");
    for fsp in [true, false] {
        let subscribe = sub(&media, fsp);
        let slots = pending_slots(&subscribe, &pack);
        for ep in 1..=3 {
            assert!(slots.contains(&(Some(1), Some(ep))), "fsp={fsp} E{ep}: {slots:?}");
        }
        let facts = facts_with_pending(SubscribeFacts::default(), [&pack], &subscribe, &media);
        for ep in 1..=3 {
            assert!(facts.get(Some(1), Some(ep)).is_some(), "fsp={fsp} placeholder E{ep}");
        }
    }
}

#[test]
fn other_season_pack_and_episodes_keep_their_own_slots() {
    let media = media();
    let subscribe = sub(&media, true);
    assert_eq!(pending_slots(&subscribe, &release::parse("Test.Show.S02.1080p")), vec![(Some(2), None)]);
    assert_eq!(
        pending_slots(&subscribe, &release::parse("Test.Show.S01E02.1080p")),
        vec![(Some(1), Some(2))]
    );
}

#[test]
fn owned_slot_with_pending_is_marked_in_flight_not_overwritten() {
    let media = media();
    let mut subscribe = sub(&media, false);
    subscribe.wash_cut = true;
    let mut facts = SubscribeFacts::default();
    facts.replace(Some(1), Some(1), QualityFact { score: 5, path: Some("/lib/e1.mkv".into()) });
    let ep = release::parse("Test.Show.S01E01.2160p");
    let facts = facts_with_pending(facts, [&ep], &subscribe, &media);
    assert!(facts.is_in_flight(Some(1), Some(1)));
    assert_eq!(facts.get(Some(1), Some(1)).unwrap().path.as_deref(), Some("/lib/e1.mkv"));
}
