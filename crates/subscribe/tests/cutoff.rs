use domain::{AtomRule, Coverage, Filter, FilterAtom, MediaId, Release, Subscribe, Torrent};
use filter::ScoredTorrent;
use subscribe::{choose, target_reached, QualityFact, SubscribeFacts};

fn tv_subscribe() -> Subscribe {
    Subscribe {
        id: domain::SubscribeId::new(),
        user_id: domain::UserId::new(),
        media_id: MediaId::new(),
        coverage: Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: Some(1),
        },
        fetch_mode: domain::FetchMode::Search,
        filter_id: domain::FilterId::new(),
        wash_cut: true,
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

fn release_with(resolution: Option<&str>, source: Option<&str>) -> Release {
    Release {
        title: "Test".into(),
        year: None,
        season: Some(1),
        episode: Some(1),
        episode_to: None,
        resolution: resolution.map(String::from),
        source: source.map(String::from),
        codec: None,
        hdr: None,
        subtitle_language: None,
        audio_language: None,
        group: None,
        confidence: domain::Confidence::High,
        season_inferred: false,
    }
}

fn scored_candidate(title: &str, resolution: Option<&str>, source: Option<&str>, score: i32) -> ScoredTorrent {
    ScoredTorrent {
        torrent: Torrent {
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
        release: release_with(resolution, source),
        score,
    }
}

#[test]
fn test_target_reached_unit_matrix() {
    // 目标2160p Remux
    let target = release_with(Some("2160p"), Some("Remux"));

    // owned 2160p WEB-DL 未达标
    let owned_webdl = release_with(Some("2160p"), Some("WEB-DL"));
    assert!(!target_reached(&owned_webdl, &target), "2160p WEB-DL 未达到 2160p Remux 目标");

    // owned 2160p Remux 达标
    let owned_remux = release_with(Some("2160p"), Some("Remux"));
    assert!(target_reached(&owned_remux, &target), "2160p Remux 达到 2160p Remux 目标");

    // 只设 source 目标时不凭空要求 resolution
    let target_source_only = release_with(None, Some("Remux"));
    let owned_no_res = release_with(None, Some("Remux"));
    assert!(target_reached(&owned_no_res, &target_source_only), "只设 source 目标且 owned 达到时达标");

    let owned_720p_remux = release_with(Some("720p"), Some("Remux"));
    assert!(target_reached(&owned_720p_remux, &target_source_only), "只设 source 目标时不凭空要求 resolution");

    let owned_webdl_no_res = release_with(None, Some("WEB-DL"));
    assert!(!target_reached(&owned_webdl_no_res, &target_source_only), "source 未达到时不达标");

    // owned 字段未知时视为未达标
    let target_res = release_with(Some("2160p"), None);
    let owned_unknown_res = release_with(None, Some("WEB-DL"));
    assert!(!target_reached(&owned_unknown_res, &target_res), "owned 缺少目标指定维度时未达标");

    // 空 target 绝不达标
    let target_empty = release_with(None, None);
    assert!(!target_reached(&owned_remux, &target_empty), "目标未声明任何维度时不达标");
}

#[test]
fn test_choose_target_2160p_with_source_ladder_not_blocked() {
    // 目标2160p、ladder仅source、owned 720p WEB-DL，2160p候选不能被 cutoff 挡住。
    // 旧逻辑下：ladder 只有 source，cutoff 借用 ladder 比较：
    // ladder_compare(owned: 720p WEB-DL, target: 2160p)
    // 比较 source: WEB-DL (level 4) vs target 无 source (level 0) -> Greater!
    // 导致旧代码误认为 existing >= target，提前停止升级！
    let subscribe = tv_subscribe();
    let mut facts = SubscribeFacts::default();
    let owned_path = "/path/to/S01E01.720p.web-dl.mkv";
    facts.replace(
        Some(1),
        Some(1),
        QualityFact {
            score: 50,
            path: Some(owned_path.into()),
        },
    );
    facts.set_quality(owned_path.into(), release_with(Some("720p"), Some("WEB-DL")));

    let wash_filter = Filter::new(
        domain::FilterId::new(),
        "cutoff-target-2160p",
        vec![
            FilterAtom {
                priority: 100,
                rule: AtomRule::WashTarget("2160p".into()),
                exclude: false,
            },
            FilterAtom {
                priority: 1,
                rule: AtomRule::UpgradeLadder("source".into()),
                exclude: false,
            },
        ],
    );

    // 候选：2160p BluRay，source 优于 owned (BluRay > WEB-DL)，且 owned 720p 未达标 2160p cutoff，
    // 那么 2160p BluRay 候选绝不能被 cutoff 挡住！
    let candidate = scored_candidate("Show.S01E01.2160p.BluRay", Some("2160p"), Some("BluRay"), 100);

    let chosen = choose(&subscribe, Some(&wash_filter), &[candidate], &facts);
    assert_eq!(chosen.len(), 1, "2160p 候选不能被 cutoff 挡住");
}

#[test]
fn test_upgrade_ladder_preserves_worse_or_equal_candidate_rejection() {
    // UpgradeLadder 只决定候选是否比 owned 更好，不用于替代 target_reached；
    // 低质量和同质量候选不因 cutoff 修改变成可替换。
    let subscribe = tv_subscribe();
    let mut facts = SubscribeFacts::default();
    let owned_path = "/path/to/S01E01.720p.web-dl.mkv";
    facts.replace(
        Some(1),
        Some(1),
        QualityFact {
            score: 50,
            path: Some(owned_path.into()),
        },
    );
    facts.set_quality(owned_path.into(), release_with(Some("720p"), Some("WEB-DL")));

    let wash_filter = Filter::new(
        domain::FilterId::new(),
        "cutoff-target-2160p",
        vec![
            FilterAtom {
                priority: 100,
                rule: AtomRule::WashTarget("2160p".into()),
                exclude: false,
            },
            FilterAtom {
                priority: 1,
                rule: AtomRule::UpgradeLadder("source".into()),
                exclude: false,
            },
        ],
    );

    // 虽然 owned 未达到 2160p 目标，但候选也是 WEB-DL（同质量），按 ladder("source") 并未优于 owned，不能被选中
    let same_quality = scored_candidate("Show.S01E01.720p.WEB-DL-RERUN", Some("720p"), Some("WEB-DL"), 60);
    // 候选是 HDTV（低质量），也不能被选中
    let lower_quality = scored_candidate("Show.S01E01.720p.HDTV", Some("720p"), Some("HDTV"), 80);

    let chosen = choose(&subscribe, Some(&wash_filter), &[same_quality, lower_quality], &facts);
    assert!(chosen.is_empty(), "低质量和同质量候选不能因为未达 cutoff 就被替换");
}
