use super::*;
use domain::{Coverage, Media, MediaId, MediaKind, Subscribe};

fn media(title: &str, original: Option<&str>) -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.into(),
        year: None,
        original_title: original.map(str::to_string),
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn release(title: &str) -> Release {
    Release {
        title: title.into(),
        year: None,
        season: None,
        episode: None,
        episode_to: None,
        resolution: None,
        source: None,
        codec: None,
        hdr: None,
        subtitle_language: None,
        audio_language: None,
        group: None,
        confidence: domain::Confidence::High,
        season_inferred: false,
    }
}

#[test]
fn chinese_keyword_does_not_bypass_identity() {
    let m = media("小羊肖恩", None);
    let release = release("Shaun.the.Sheep.The.Farmer.s.Llamas.2026");
    let keywords = vec!["小羊肖恩".to_string()];
    assert!(!matches_media(&m, &release, &keywords));
}

#[test]
fn english_keyword_still_matches_english_release() {
    let m = media("小羊肖恩", Some("Shaun the Sheep"));
    let release = release("Shaun.the.Sheep.The.Farmer.s.Llamas.2026");
    let keywords = vec!["小羊肖恩".to_string(), "Shaun the Sheep".to_string()];
    assert!(matches_media(&m, &release, &keywords));
}

#[test]
fn unrelated_candidate_is_rejected_with_cjk_keyword() {
    let m = media("小羊肖恩", Some("Shaun the Sheep"));
    let release = release("Frozen.II.2019.2160p");
    let keywords = vec!["小羊肖恩".to_string()];
    assert!(!matches_media(&m, &release, &keywords));
}

#[test]
fn short_ascii_title_requires_a_token_match() {
    let m = media("It", None);
    assert!(!matches_media(&m, &release("Titanic.1997.1080p"), &[]));
    assert!(matches_media(&m, &release("It.2017.1080p"), &[]));
}

#[test]
fn media_year_and_kind_are_checked_for_subscribe_candidates() {
    let mut m = media("Arrival", None);
    m.year = Some(2016);
    let subscribe = Subscribe {
        id: domain::SubscribeId::new(),
        user_id: domain::UserId::new(),
        media_id: m.id,
        coverage: Coverage::Movie,
        fetch_mode: domain::FetchMode::Search,
        filter_id: domain::FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };
    let mut wrong_year = release("Arrival.2016");
    wrong_year.year = Some(2015);
    assert!(!candidate_matches_subscribe(&subscribe, &m, &wrong_year));
    let mut tv_release = release("Arrival.S01E01");
    tv_release.year = Some(2016);
    tv_release.season = Some(1);
    tv_release.episode = Some(1);
    assert!(!candidate_matches_subscribe(&subscribe, &m, &tv_release));
}

#[test]
fn tv_candidate_must_match_the_subscribed_episode_window() {
    let mut m = media("The Long Watch", None);
    m.kind = MediaKind::Tv;
    let subscribe = Subscribe {
        id: domain::SubscribeId::new(),
        user_id: domain::UserId::new(),
        media_id: m.id,
        coverage: Coverage::Tv {
            season: 1,
            episode_from: 2,
            episode_to: Some(3),
        },
        fetch_mode: domain::FetchMode::Search,
        filter_id: domain::FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };

    let mut wrong_season = release("The Long Watch S02E02");
    wrong_season.season = Some(2);
    wrong_season.episode = Some(2);
    assert!(!candidate_matches_subscribe(&subscribe, &m, &wrong_season));

    let mut matching_episode = release("The Long Watch S01E03");
    matching_episode.season = Some(1);
    matching_episode.episode = Some(3);
    assert!(candidate_matches_subscribe(
        &subscribe,
        &m,
        &matching_episode
    ));
}

fn tv_subscribe(media_id: domain::MediaId) -> Subscribe {
    Subscribe {
        id: domain::SubscribeId::new(),
        user_id: domain::UserId::new(),
        media_id,
        coverage: Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: Some(2),
        },
        fetch_mode: domain::FetchMode::Search,
        filter_id: domain::FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    }
}

fn scored(title: &str, from: u32, to: u32, score: i32) -> ScoredTorrent {
    ScoredTorrent {
        torrent: domain::Torrent {
            site_id: domain::SiteId::new(),
            title: title.into(),
            enclosure: format!("https://pt.example/{title}"),
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
        },
        release: Release {
            title: "Show".into(),
            year: None,
            season: Some(1),
            episode: Some(from),
            episode_to: Some(to),
            resolution: None,
            source: None,
            codec: None,
            hdr: None,
            subtitle_language: None,
            audio_language: None,
            group: None,
            confidence: domain::Confidence::High,
            season_inferred: false,
        },
        score,
    }
}

#[test]
fn h264_and_h265_aliases_rank_with_x264_and_hevc() {
    let mut owned = release("owned");
    owned.codec = Some("h264".into());
    let mut target = release("target");
    target.codec = Some("x264".into());
    assert!(target_reached(&owned, &target));
    owned.codec = Some("h265".into());
    target.codec = Some("hevc".into());
    assert!(target_reached(&owned, &target));
}

#[test]
fn pack_outside_the_window_does_not_download_with_the_in_window_winner() {
    let mut subscribe = tv_subscribe(domain::MediaId::new());
    subscribe.coverage = Coverage::Tv {
        season: 1,
        episode_from: 2,
        episode_to: Some(2),
    };
    let pack = scored("Show.S01E01-E02", 1, 2, 10);
    let single = scored("Show.S01E02", 2, 2, 20);
    let chosen = choose(
        &subscribe,
        None,
        &[pack, single],
        &SubscribeFacts::default(),
    );
    assert_eq!(
        chosen.len(),
        1,
        "窗口外的集数不能再拉来一份种子: {chosen:?}"
    );
    assert!(chosen[0].torrent.title.contains("S01E02"));
}

#[test]
fn huge_release_range_does_not_allocate_unbounded_choose_slots() {
    let media_id = domain::MediaId::new();
    let subscribe = tv_subscribe(media_id);
    let huge = scored("Show.S01E01-E4294967295", 1, u32::MAX, 10);
    let precise = scored("Show.S01E02", 2, 2, 20);
    // M1: overlapping candidates never download together; with PreferEpisode
    // the higher-scoring single still wins its slot.
    let chosen = super::choose_with_policy(
        &subscribe,
        None,
        &[huge.clone(), precise.clone()],
        &SubscribeFacts::default(),
        super::OverlapPolicy::PreferEpisode,
    );
    assert!(
        chosen.iter().any(|c| c.torrent.title.contains("S01E02")),
        "higher-scoring single episode must still win its slot: {chosen:?}"
    );
    let chosen = choose(
        &subscribe,
        None,
        &[huge, precise],
        &SubscribeFacts::default(),
    );
    assert_eq!(chosen.len(), 1, "capped range must not explode chosen set");
}

#[test]
fn ladder_uses_probed_quality_not_renamed_filename() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("Test Show - S01E01.mkv");
    std::fs::write(
        &path,
        r#"{"resolution":"2160p","codec":"hevc","hdr":"hdr10"}"#,
    )
    .unwrap();
    let mut subscribe = tv_subscribe(domain::MediaId::new());
    subscribe.wash_cut = true;
    let mut facts = SubscribeFacts::default();
    facts.replace(
        Some(1),
        Some(1),
        crate::QualityFact {
            score: 50,
            path: Some(path.display().to_string()),
        },
    );
    facts.set_quality(
        path.display().to_string(),
        release::parse("Test.Show.S01E01.2160p.HEVC.HDR10"),
    );
    let mut candidate = scored("Test.Show.S01E01.720p", 1, 1, 90);
    candidate.release.resolution = Some("720p".into());
    let wash = domain::Filter::new(
        domain::FilterId::new(),
        "ladder",
        vec![domain::FilterAtom {
            priority: 1,
            rule: domain::AtomRule::UpgradeLadder("resolution".into()),
            exclude: false,
        }],
    );
    assert!(!should_replace_slots(
        &subscribe,
        Some(&wash),
        &facts,
        &candidate,
        &[(Some(1), Some(1))]
    ));
}

#[test]
fn unknown_source_blocks_resolution_first_destructive_wash_cut() {
    let mut subscribe = tv_subscribe(domain::MediaId::new());
    subscribe.wash_cut = true;
    subscribe.keep_old_versions = false;
    let mut facts = SubscribeFacts::default();
    let owned_path = "/library/Test.Show.S01E01.1080p.mkv";
    facts.replace(
        Some(1),
        Some(1),
        crate::QualityFact {
            score: 40,
            path: Some(owned_path.into()),
        },
    );
    let mut owned = release("Test.Show.S01E01.1080p");
    owned.resolution = Some("1080p".into());
    owned.source = None;
    facts.set_quality(owned_path.into(), owned);
    let wash = domain::Filter::new(
        domain::FilterId::new(),
        "ladder",
        vec![domain::FilterAtom {
            priority: 1,
            rule: domain::AtomRule::UpgradeLadder("resolution,source".into()),
            exclude: false,
        }],
    );
    let mut candidate = scored("Test.Show.S01E01.2160p.BluRay", 1, 1, 40);
    candidate.release.resolution = Some("2160p".into());
    candidate.release.source = Some("bluray".into());
    assert!(
        !should_replace_slots(
            &subscribe,
            Some(&wash),
            &facts,
            &candidate,
            &[(Some(1), Some(1))]
        ),
        "已有来源未知时，更高分辨率不能单独批准删除旧文件"
    );
}

#[test]
fn unknown_quality_does_not_authorize_destructive_wash_cut() {
    let mut subscribe = tv_subscribe(domain::MediaId::new());
    subscribe.wash_cut = true;
    subscribe.keep_old_versions = false;
    let mut facts = SubscribeFacts::default();
    facts.replace(
        Some(1),
        Some(1),
        crate::QualityFact {
            score: 10,
            path: Some("/library/Show.S01E01.mkv".into()),
        },
    );
    let mut candidate = scored("Show.S01E01.720p.HDTV", 1, 1, 80);
    candidate.release.resolution = Some("720p".into());
    candidate.release.source = Some("hdtv".into());
    candidate.release.codec = Some("x264".into());
    let ladder = |raw: &str| {
        domain::Filter::new(
            domain::FilterId::new(),
            "ladder",
            vec![domain::FilterAtom {
                priority: 1,
                rule: domain::AtomRule::UpgradeLadder(raw.into()),
                exclude: false,
            }],
        )
    };
    for raw in ["resolution", "codec", "hdr"] {
        assert!(
            choose(
                &subscribe,
                Some(&ladder(raw)),
                std::slice::from_ref(&candidate),
                &facts
            )
            .is_empty(),
            "{raw} 未知时不能批准破坏性替换"
        );
    }
    assert!(
        choose(&subscribe, None, std::slice::from_ref(&candidate), &facts).is_empty(),
        "没有阶梯也不能用默认分数批准未知质量的删除"
    );
}

#[test]
fn both_unknown_ladder_dimension_blocks_later_resolution_upgrade() {
    let mut subscribe = tv_subscribe(domain::MediaId::new());
    subscribe.wash_cut = true;
    let mut facts = SubscribeFacts::default();
    let path = "/library/Show.S01E01.mkv";
    facts.replace(
        Some(1),
        Some(1),
        crate::QualityFact {
            score: 10,
            path: Some(path.into()),
        },
    );
    let mut old = release("Show");
    old.resolution = Some("720p".into());
    facts.set_quality(path.into(), old);
    let mut candidate = scored("Show.S01E01.1080p", 1, 1, 80);
    candidate.release.resolution = Some("1080p".into());
    let wash = domain::Filter::new(
        domain::FilterId::new(),
        "source first",
        vec![domain::FilterAtom {
            priority: 1,
            rule: domain::AtomRule::UpgradeLadder("source,resolution".into()),
            exclude: false,
        }],
    );
    assert!(choose(&subscribe, Some(&wash), &[candidate], &facts).is_empty());
}

#[test]
fn known_quality_score_upgrade_still_works_without_ladder() {
    let mut subscribe = tv_subscribe(domain::MediaId::new());
    subscribe.wash_cut = true;
    let mut facts = SubscribeFacts::default();
    let path = "/library/Show.S01E01.mkv";
    facts.replace(
        Some(1),
        Some(1),
        crate::QualityFact {
            score: 10,
            path: Some(path.into()),
        },
    );
    let mut old = release("Show");
    old.resolution = Some("720p".into());
    facts.set_quality(path.into(), old);
    let mut candidate = scored("Show.S01E01.1080p", 1, 1, 80);
    candidate.release.resolution = Some("1080p".into());
    assert_eq!(choose(&subscribe, None, &[candidate], &facts).len(), 1);
}

#[test]
fn cutoff_and_ladder_share_filename_fallback_without_overwriting_probe() {
    let mut subscribe = tv_subscribe(domain::MediaId::new());
    subscribe.wash_cut = true;
    let path = "/library/Show.S01E01.1080p.Remux.mkv";
    let mut facts = SubscribeFacts::default();
    facts.replace(
        Some(1),
        Some(1),
        crate::QualityFact {
            score: 10,
            path: Some(path.into()),
        },
    );
    let mut old = release("Show");
    old.resolution = Some("2160p".into());
    facts.set_quality(path.into(), old);
    let mut candidate = scored("Show.S01E01.2160p.Remux", 1, 1, 80);
    candidate.release.resolution = Some("2160p".into());
    candidate.release.source = Some("remux".into());
    let wash = domain::Filter::new(
        domain::FilterId::new(),
        "cutoff",
        vec![domain::FilterAtom {
            priority: 1,
            rule: domain::AtomRule::WashTarget("2160p remux".into()),
            exclude: false,
        }],
    );
    assert!(
        choose(&subscribe, Some(&wash), &[candidate], &facts).is_empty(),
        "来源由文件名补全，探测分辨率优先；已达截止线不能按分数继续升级"
    );
}

#[test]
fn highest_scoring_candidate_not_matching_ladder_allows_eligible_candidate() {
    let mut subscribe = tv_subscribe(domain::MediaId::new());
    subscribe.wash_cut = true;
    let mut facts = SubscribeFacts::default();
    let owned_path = "/path/to/S01E01.1080p.mkv";
    facts.replace(
        Some(1),
        Some(1),
        crate::QualityFact {
            score: 80,
            path: Some(owned_path.into()),
        },
    );
    let mut owned_release = release("Test.Show.S01E01.1080p");
    owned_release.resolution = Some("1080p".into());
    facts.set_quality(owned_path.into(), owned_release);

    let wash = domain::Filter::new(
        domain::FilterId::new(),
        "ladder",
        vec![domain::FilterAtom {
            priority: 1,
            rule: domain::AtomRule::UpgradeLadder("resolution".into()),
            exclude: false,
        }],
    );

    let mut ineligible = scored("Test.Show.S01E01.720p", 1, 1, 100);
    ineligible.release.resolution = Some("720p".into());
    let mut eligible = scored("Test.Show.S01E01.2160p", 1, 1, 50);
    eligible.release.resolution = Some("2160p".into());

    let chosen = choose(&subscribe, Some(&wash), &[ineligible, eligible], &facts);
    assert_eq!(chosen.len(), 1, "必须选出符合升级阶梯的有效候选");
    assert!(chosen[0].torrent.title.contains("2160p"));
}

#[test]
fn wash_target_cutoff_prevents_further_upgrades_when_target_reached() {
    let mut subscribe = tv_subscribe(domain::MediaId::new());
    subscribe.wash_cut = true;
    let mut facts = SubscribeFacts::default();
    let owned_path = "/path/to/S01E01.1080p.mkv";
    facts.replace(
        Some(1),
        Some(1),
        crate::QualityFact {
            score: 100,
            path: Some(owned_path.into()),
        },
    );
    let mut owned_release = release("Test.Show.S01E01.1080p");
    owned_release.resolution = Some("1080p".into());
    facts.set_quality(owned_path.into(), owned_release);

    let wash = domain::Filter::new(
        domain::FilterId::new(),
        "target-1080p",
        vec![
            domain::FilterAtom {
                priority: 100,
                rule: domain::AtomRule::WashTarget("1080p".into()),
                exclude: false,
            },
            domain::FilterAtom {
                priority: 1,
                rule: domain::AtomRule::UpgradeLadder("resolution".into()),
                exclude: false,
            },
        ],
    );

    let mut higher_res = scored("Test.Show.S01E01.2160p", 1, 1, 150);
    higher_res.release.resolution = Some("2160p".into());

    let chosen = choose(&subscribe, Some(&wash), &[higher_res], &facts);
    assert!(
        chosen.is_empty(),
        "达到 WashTarget 截止线后必须停止继续升级"
    );
}

#[test]
fn inferred_season_never_matches_a_later_season_subscribe() {
    let mut m = media("The Long Watch", None);
    m.kind = MediaKind::Tv;
    let mut subscribe = tv_subscribe(m.id);
    subscribe.coverage = Coverage::Tv {
        season: 2,
        episode_from: 1,
        episode_to: Some(2),
    };
    let mut guessed = release("The Long Watch Episode 1");
    guessed.season = Some(2);
    guessed.episode = Some(1);
    guessed.season_inferred = true;
    assert!(!candidate_matches_subscribe(&subscribe, &m, &guessed));
    guessed.season_inferred = false;
    assert!(candidate_matches_subscribe(&subscribe, &m, &guessed));
}

fn ten_episode_subscribe() -> (Media, Subscribe) {
    let mut m = media("Show", None);
    m.kind = MediaKind::Tv;
    let mut subscribe = tv_subscribe(m.id);
    subscribe.coverage = Coverage::Tv {
        season: 1,
        episode_from: 1,
        episode_to: Some(10),
    };
    (m, subscribe)
}

#[test]
fn overlapping_pack_and_single_choose_exactly_one_per_policy() {
    let (_, subscribe) = ten_episode_subscribe();
    let pack = scored("pack", 1, 10, 90);
    let single = scored("e06", 6, 6, 95);
    let facts = SubscribeFacts::default();
    let both = [pack.clone(), single.clone()];
    let pick = |policy| {
        super::choose_with_policy(&subscribe, None, &both, &facts, policy)
            .into_iter()
            .map(|c| c.torrent.title)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        pick(super::OverlapPolicy::PreferPack),
        vec!["pack".to_string()]
    );
    assert_eq!(
        pick(super::OverlapPolicy::PreferEpisode),
        vec!["e06".to_string()]
    );
}

#[test]
fn non_overlapping_singles_are_each_chosen_and_best_score_wins_a_slot() {
    let (_, subscribe) = ten_episode_subscribe();
    let picks = choose(
        &subscribe,
        None,
        &[
            scored("e01", 1, 1, 50),
            scored("e02", 2, 2, 50),
            scored("e01-better", 1, 1, 70),
        ],
        &SubscribeFacts::default(),
    );
    let mut titles: Vec<_> = picks.into_iter().map(|c| c.torrent.title).collect();
    titles.sort();
    assert_eq!(titles, vec!["e01-better".to_string(), "e02".to_string()]);
}

#[test]
fn m9_scanned_2160p_is_not_replaced_by_high_scoring_1080p_without_ladder() {
    let mut subscribe = tv_subscribe(domain::MediaId::new());
    subscribe.wash_cut = true;
    let mut facts = SubscribeFacts::default();
    let path = "/library/Show.S01E01.mkv";
    facts.replace(
        Some(1),
        Some(1),
        crate::QualityFact {
            score: 0, // scanned into the library: no filter score
            path: Some(path.into()),
        },
    );
    let mut old = release("Show");
    old.resolution = Some("2160p".into());
    facts.set_quality(path.into(), old);
    let mut candidate = scored("Show.S01E01.1080p", 1, 1, 90);
    candidate.release.resolution = Some("1080p".into());
    assert!(choose(&subscribe, None, &[candidate], &facts).is_empty());
}

#[test]
fn m9_same_quality_falls_back_to_score_tie_break() {
    let mut subscribe = tv_subscribe(domain::MediaId::new());
    subscribe.wash_cut = true;
    let mut facts = SubscribeFacts::default();
    let path = "/library/Show.S01E01.mkv";
    facts.replace(
        Some(1),
        Some(1),
        crate::QualityFact {
            score: 10,
            path: Some(path.into()),
        },
    );
    let mut old = release("Show");
    old.resolution = Some("1080p".into());
    facts.set_quality(path.into(), old);
    let mut better = scored("Show.S01E01.1080p.A", 1, 1, 20);
    better.release.resolution = Some("1080p".into());
    assert_eq!(choose(&subscribe, None, &[better.clone()], &facts).len(), 1);
    better.score = 5;
    assert!(choose(&subscribe, None, &[better], &facts).is_empty());
}

fn bare_pack(size: u64) -> ScoredTorrent {
    let mut pack = scored("Show.S01.SP", 1, 1, 50);
    pack.release.episode = None;
    pack.release.episode_to = None;
    pack.torrent.size_bytes = Some(size);
    pack
}

#[test]
fn l1_small_bare_season_is_not_a_pack() {
    let mut subscribe = tv_subscribe(domain::MediaId::new());
    subscribe.full_season_pack = true;
    let mut single = scored("Show.S01E01", 1, 1, 10);
    single.torrent.size_bytes = Some(2_000_000_000);
    let facts = SubscribeFacts::default();
    let small = bare_pack(300_000_000);
    assert!(choose(&subscribe, None, &[small, single.clone()], &facts).is_empty());
    let big = bare_pack(3_500_000_000);
    assert_eq!(choose(&subscribe, None, &[big, single], &facts).len(), 1);
}

#[test]
fn l2_wider_range_pack_is_accepted() {
    let mut subscribe = tv_subscribe(domain::MediaId::new());
    subscribe.full_season_pack = true;
    subscribe.coverage = Coverage::Tv {
        season: 1,
        episode_from: 1,
        episode_to: Some(12),
    };
    let facts = SubscribeFacts::default();
    let wide = scored("Show.S01E01-E13", 1, 13, 50);
    assert_eq!(choose(&subscribe, None, &[wide], &facts).len(), 1);
    let narrow = scored("Show.S01E01-E11", 1, 11, 50);
    assert!(choose(&subscribe, None, &[narrow], &facts).is_empty());
}
