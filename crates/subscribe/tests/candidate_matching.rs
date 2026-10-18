use domain::{
    Confidence, Coverage, FetchMode, Media, MediaId, MediaKind, Release, Subscribe, SubscribeId,
    UserId,
};

fn media() -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "The Long Watch: A New Chapter".into(),
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

fn subscribe(media: &Media) -> Subscribe {
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
        filter_id: domain::FilterId::new(),
        wash_cut: false,
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

#[test]
fn pending_release_matches_the_short_title_used_to_find_it() {
    let media = media();
    let subscribe = subscribe(&media);
    let release = Release {
        title: "The Long Watch S01E01 1080p WEB-DL".into(),
        year: None,
        season: Some(1),
        episode: Some(1),
        episode_to: None,
        resolution: Some("1080p".into()),
        source: Some("WEB-DL".into()),
        codec: None,
        hdr: None,
        subtitle_language: None,
        audio_language: None,
        group: None,
        confidence: Confidence::High,
        season_inferred: false,
    };

    assert!(subscribe::candidate_matches_subscribe(
        &subscribe, &media, &release
    ));
}
