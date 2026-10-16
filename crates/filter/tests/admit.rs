use std::sync::Arc;

use domain::{AtomRule, Filter, FilterAtom, FilterId, Site, SiteId, Torrent};
use indexer::{FetchRequest, Fetcher, Indexer, IndexerError, ProfileSet};

fn demo_site() -> Site {
    Site {
        id: SiteId::new(),
        name: "demo".into(),
        url: "https://pt.example/".into(),
        profile_id: "demo".into(),
        cookie: None,
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
    }
}

struct Injected(String);

impl Fetcher for Injected {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Ok(self.0.clone())
    }
}

fn search_demo() -> Vec<Torrent> {
    let site = demo_site();
    let html = include_str!("../../indexer/tests/fixtures/nexusphp.html");
    let indexer = Indexer::new(
        ProfileSet::load(None).unwrap(),
        Arc::new(Injected(html.to_string())),
    );
    let outcome = indexer.search(&[site], "matrix");
    assert!(outcome.failures.is_empty());
    outcome.torrents
}

#[test]
fn exclude_only_filter_admits_non_hr_and_rejects_hr() {
    let mut clean = search_demo().remove(0);
    clean.hr = false;
    let mut hr = clean.clone();
    hr.hr = true;
    let filter = Filter {
        id: FilterId::new(),
        name: "no HR".into(),
        atoms: vec![FilterAtom {
            priority: 100,
            rule: AtomRule::Hr,
            exclude: true,
        }],
        keep_old_versions: false,
};
    let outcome = filter::admit(vec![clean, hr], &filter);
    assert_eq!(outcome.admitted.len(), 1);
    assert!(!outcome.admitted[0].torrent.hr);
    assert_eq!(outcome.rejected.len(), 1);
}

#[test]
fn codec_alias_filter_admits_hevc_and_rejects_avc() {
    let mut hevc = search_demo().remove(0);
    hevc.title = "The.Matrix.1999.2160p.BluRay.x265".into();
    let mut avc = hevc.clone();
    avc.title = "The.Matrix.1999.2160p.BluRay.x264".into();
    let filter = Filter {
        id: FilterId::new(),
        name: "HEVC".into(),
        atoms: vec![FilterAtom {
            priority: 90,
            rule: AtomRule::Codec("H.265".into()),
            exclude: false,
        }],
        keep_old_versions: false,
};
    let outcome = filter::admit(vec![hevc, avc], &filter);
    assert_eq!(outcome.admitted.len(), 1);
    assert_eq!(outcome.admitted[0].score, 90);
    assert_eq!(outcome.rejected.len(), 1);
}

fn quality_filter() -> Filter {
    Filter {
        id: FilterId::new(),
        name: "uhd-bluray".into(),
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
            FilterAtom {
                priority: 10,
                rule: AtomRule::Free,
                exclude: false,
            },
            FilterAtom {
                priority: 5,
                rule: AtomRule::Hr,
                exclude: false,
            },
            FilterAtom {
                priority: 80,
                rule: AtomRule::TitleMatch("Matrix".into()),
                exclude: false,
            },
        ],
        keep_old_versions: false,
}
}

#[test]
fn search_path_admits_survivors_with_max_matched_priority() {
    let torrents = search_demo();
    let outcome = filter::admit(torrents, &quality_filter());

    assert_eq!(outcome.admitted.len(), 1);
    let scored = &outcome.admitted[0];
    assert_eq!(
        scored.torrent.title,
        "The.Matrix.1999.2160p.BluRay.x265-GROUP"
    );
    assert_eq!(scored.release.resolution.as_deref(), Some("2160p"));
    assert_eq!(scored.release.source.as_deref(), Some("BluRay"));
    assert_eq!(scored.score, 100);
}

#[test]
fn junk_titles_that_match_no_atom_are_rejected_with_score_zero() {
    let mut torrents = search_demo();
    torrents.push(Torrent {
        site_id: torrents[0].site_id,
        title: "Some.Movie.2020.480p.CAM.x264-JUNK".into(),
        enclosure: "https://pt.example/download.php?id=9".into(),
        size_bytes: Some(700_000_000),
        seeders: Some(1),
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
    });

    let outcome = filter::admit(torrents, &quality_filter());
    assert_eq!(outcome.admitted.len(), 1);
    assert_eq!(outcome.rejected.len(), 1);
    assert_eq!(
        outcome.rejected[0].torrent.title,
        "Some.Movie.2020.480p.CAM.x264-JUNK"
    );
    assert_eq!(outcome.rejected[0].score, 0);
}

#[test]
fn low_confidence_release_is_not_an_auto_download_candidate() {
    let mut torrents = search_demo();
    torrents.push(Torrent {
        site_id: torrents[0].site_id,
        title: "asdf".into(),
        enclosure: "https://pt.example/download.php?id=10".into(),
        size_bytes: None,
        seeders: None,
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
    });

    let outcome = filter::admit(torrents, &quality_filter());
    assert_eq!(outcome.admitted.len(), 1);
    assert_eq!(outcome.skipped_low_confidence.len(), 1);
    assert_eq!(outcome.skipped_low_confidence[0].title, "asdf");
    assert!(!outcome.admitted.iter().any(|c| c.torrent.title == "asdf"));
}

#[test]
fn override_release_scores_by_corrected_fields() {
    let torrents = search_demo();
    let mut junk = torrents[0].clone();
    junk.title = "asdf".into();
    junk.enclosure = "https://pt.example/download.php?id=11".into();
    let mut overridden = release::parse("The.Matrix.1999.1080p.WEB-DL.x264-GROUP");
    overridden.resolution = Some("2160p".into());
    overridden.confidence = domain::Confidence::High;

    let plain = filter::admit(vec![junk.clone()], &quality_filter());
    assert!(plain.admitted.is_empty());

    let outcome = filter::admit_scored(vec![(junk, Some(overridden.clone()))], &quality_filter());
    assert_eq!(outcome.admitted.len(), 1);
    let scored = &outcome.admitted[0];
    assert_eq!(scored.release.resolution.as_deref(), Some("2160p"));
    assert_eq!(scored.score, 100);
}

fn torrent(title: &str, size_mb: Option<u64>, seeders: Option<u32>) -> Torrent {
    Torrent {
        site_id: SiteId::new(),
        title: title.into(),
        enclosure: format!("https://pt.example/download.php?id={}", title.len()),
        size_bytes: size_mb.map(|mb| mb * 1024 * 1024),
        seeders,
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

fn filter_with(atoms: Vec<FilterAtom>) -> Filter {
    Filter {
        id: FilterId::new(),
        name: "test".into(),
        atoms,
        keep_old_versions: false,
    }
}

#[test]
fn min_seeders_atom_admits_only_torrents_above_threshold() {
    let filter = filter_with(vec![FilterAtom {
        priority: 100,
        rule: AtomRule::MinSeeders(10),
        exclude: false,
    }]);
    let few = torrent("Few.Seeders.2024.1080p.WEB-DL", Some(900), Some(3));
    let many = torrent("Many.Seeders.2024.1080p.WEB-DL", Some(900), Some(20));
    let unknown = torrent("Unknown.Seeders.2024.1080p.WEB-DL", Some(900), None);

    let outcome = filter::admit(vec![few, many, unknown], &filter);
    assert_eq!(outcome.admitted.len(), 1, "做种数未知视为不命中");
    assert!(outcome.admitted[0].torrent.title.contains("Many"));
    assert_eq!(outcome.admitted[0].score, 100);
}

#[test]
fn size_atom_admits_only_torrents_in_range() {
    let filter = filter_with(vec![FilterAtom {
        priority: 90,
        rule: AtomRule::Size {
            min_mb: Some(500),
            max_mb: Some(2000),
        },
        exclude: false,
    }]);
    let small = torrent("Small.File.2024.1080p.WEB-DL", Some(200), Some(20));
    let big = torrent("Big.File.2024.1080p.WEB-DL", Some(9000), Some(20));
    let good = torrent("Just.Right.2024.1080p.WEB-DL", Some(900), Some(20));

    let outcome = filter::admit(vec![small, big, good], &filter);
    assert_eq!(
        outcome.admitted.len(),
        1,
        "只有 500MB≤size≤2000MB 的候选命中"
    );
    assert!(outcome.admitted[0].torrent.title.contains("Just.Right"));
    assert_eq!(outcome.rejected.len(), 2);
}

#[test]
fn size_atom_normalizes_inverted_bounds() {
    let filter = filter_with(vec![FilterAtom {
        priority: 90,
        rule: AtomRule::Size {
            min_mb: Some(2000),
            max_mb: Some(500),
        },
        exclude: false,
    }]);
    let good = torrent("Just.Right.2024.1080p.WEB-DL", Some(900), Some(20));
    let outcome = filter::admit(vec![good], &filter);
    assert_eq!(
        outcome.admitted.len(),
        1,
        "即使 min_mb 与 max_mb 颠倒，自动纠正后仍正常命中"
    );
}

#[test]
fn size_atom_amortizes_full_season_pack_per_episode() {
    let filter = filter_with(vec![FilterAtom {
        priority: 100,
        rule: AtomRule::Size {
            min_mb: Some(400),
            max_mb: None,
        },
        exclude: false,
    }]);
    // S01E01-E10 整季 3000MB → 每集约 300MB，低于 400MB 下限 → 拒。
    let pack = torrent("Show.2024.S01E01-E10.1080p.WEB-DL", Some(3000), Some(30));
    let outcome = filter::admit(vec![pack], &filter);
    assert!(outcome.admitted.is_empty(), "整季包按每集均摊应低于下限");

    // 单集 800MB → 命中。
    let single = torrent("Show.2024.S01E01.1080p.WEB-DL", Some(800), Some(30));
    let outcome = filter::admit(vec![single], &filter);
    assert_eq!(outcome.admitted.len(), 1);
}

#[test]
fn size_atom_caps_huge_episode_range_before_amortizing() {
    let filter = filter_with(vec![FilterAtom {
        priority: 100,
        rule: AtomRule::Size {
            min_mb: Some(1),
            max_mb: Some(2),
        },
        exclude: false,
    }]);
    // 若按 E01-E4294967295 均摊，体积会趋近 0 而被拒；上限 1000 集后每集约 2MB，应命中。
    let pack = torrent(
        "Show.2024.S01E01-E4294967295.1080p.WEB-DL",
        Some(2000),
        Some(30),
    );
    let outcome = filter::admit(vec![pack], &filter);
    assert_eq!(
        outcome.admitted.len(),
        1,
        "huge range must amortize over the 1000-episode cap: {:?}",
        outcome.rejected.len()
    );
}

#[test]
fn subtitle_and_audio_language_atoms_match_normalized_languages() {
    let filter = filter_with(vec![
        FilterAtom {
            priority: 100,
            rule: AtomRule::SubtitleLanguage("zh".into()),
            exclude: false,
        },
        FilterAtom {
            priority: 90,
            rule: AtomRule::AudioLanguage("cmn".into()),
            exclude: false,
        },
    ]);
    let chs = torrent("Movie.2024.1080p.CHS.国语.WEB-DL", Some(800), Some(20));
    let english = torrent("Movie.2024.1080p.1080p.WEB-DL.ENG.ENG", Some(800), Some(20));
    let outcome = filter::admit(vec![chs, english], &filter);
    assert_eq!(outcome.admitted.len(), 1, "只有带中字 + 国语的候选命中");
    assert!(outcome.admitted[0].torrent.title.contains("CHS"));
    assert_eq!(
        outcome.admitted[0].score, 100,
        "两个原子都命中，取最高优先级"
    );
}

#[test]
fn site_atom_prioritizes_the_named_site_but_keeps_other_sites_as_fallback() {
    let target = SiteId::new();
    let filter = filter_with(vec![FilterAtom {
        priority: 100,
        rule: AtomRule::Site(target.to_string()),
        exclude: false,
    }]);
    let mut a = torrent("Movie.2024.1080p.WEB-DL", Some(800), Some(20));
    a.site_id = target;
    let mut b = torrent("Other.2024.1080p.WEB-DL", Some(800), Some(20));
    b.site_id = SiteId::new();
    let outcome = filter::admit(vec![a, b], &filter);
    assert_eq!(outcome.admitted.len(), 2);
    assert_eq!(outcome.admitted[0].torrent.site_id, target);
    assert!(outcome.admitted[0].score > outcome.admitted[1].score);
}

#[test]
fn exclude_atom_rejects_matching_torrent_even_with_high_priority_include() {
    let filter = filter_with(vec![
        FilterAtom {
            priority: 100,
            rule: AtomRule::TitleMatch("WEB-DL".into()),
            exclude: false,
        },
        FilterAtom {
            priority: 200,
            rule: AtomRule::TitleMatch("NF".into()),
            exclude: true,
        },
    ]);
    let nf = torrent("Movie.2024.NF.WEB-DL", Some(800), Some(20));
    let other = torrent("Movie.2024.AMZN.WEB-DL", Some(800), Some(20));
    let outcome = filter::admit(vec![nf, other], &filter);
    assert_eq!(
        outcome.admitted.len(),
        1,
        "NF 黑名单命中即排除，即使 WEB-DL 白名单得分"
    );
    assert!(outcome.admitted[0].torrent.title.contains("AMZN"));
}

#[test]
fn hdr_atom_matches_release_hdr_field() {
    let filter = filter_with(vec![FilterAtom {
        priority: 100,
        rule: AtomRule::Hdr("DV".into()),
        exclude: false,
    }]);
    let dv = torrent("Movie.2024.1080p.BluRay.DV.WEB-DL", Some(800), Some(20));
    let sdr = torrent("Movie.2024.1080p.BluRay.WEB-DL", Some(800), Some(20));
    let outcome = filter::admit(vec![dv, sdr], &filter);
    assert_eq!(outcome.admitted.len(), 1);
    assert_eq!(outcome.admitted[0].score, 100);
}

#[test]
fn resolution_atom_and_dv_and_size_combined_rule_matches() {
    let filter = filter_with(vec![
        FilterAtom {
            priority: 90,
            rule: AtomRule::Resolution("2160p".into()),
            exclude: false,
        },
        FilterAtom {
            priority: 100,
            rule: AtomRule::Hdr("DV".into()),
            exclude: false,
        },
        FilterAtom {
            priority: 80,
            rule: AtomRule::Size {
                min_mb: Some(1000),
                max_mb: Some(50000),
            },
            exclude: false,
        },
    ]);
    // 命中 4k (2160p) + DV + 大小在 20GB (20480MB) 范围
    let perfect = torrent(
        "Jiao.Feng.2024.S01E13.4k.DV.HDR.WEB-DL-GROUP",
        Some(20480 * 1024 * 1024),
        Some(15),
    );
    // 720p 且无 DV，且只有 300MB（低于 1000MB）
    let low = torrent(
        "Jiao.Feng.2024.S01E13.720p.HDTV-GROUP",
        Some(300 * 1024 * 1024),
        Some(5),
    );

    let outcome = filter::admit(vec![perfect, low], &filter);
    assert_eq!(outcome.admitted.len(), 1);
    assert_eq!(
        outcome.admitted[0].release.resolution.as_deref(),
        Some("2160p")
    );
    assert_eq!(outcome.admitted[0].release.hdr.as_deref(), Some("DV"));
    assert_eq!(outcome.admitted[0].score, 100);
}

#[test]
fn source_matching_handles_ui_hyphen_and_family_variants() {
    let t_bluray = torrent("Movie.1080p.BluRay.x264", None, None);
    let t_webrip = torrent("Movie.1080p.WEBRip.x264", None, None);
    let t_hdtv = torrent("Show.720p.HDTV.x264", None, None);

    // 1. UI 传的 "blu-ray" 必须命中解析得到的 "BluRay"
    let f_bluray = filter_with(vec![FilterAtom {
        priority: 80,
        rule: AtomRule::Source("blu-ray".into()),
        exclude: false,
    }]);
    assert_eq!(
        filter::admit(vec![t_bluray.clone()], &f_bluray)
            .admitted
            .len(),
        1
    );

    // 2. UI 传的 "rip" 必须命中 "WEBRip"
    let f_rip = filter_with(vec![FilterAtom {
        priority: 80,
        rule: AtomRule::Source("rip".into()),
        exclude: false,
    }]);
    assert_eq!(
        filter::admit(vec![t_webrip.clone()], &f_rip).admitted.len(),
        1
    );

    // 3. UI 传的 "tv" 必须命中 "HDTV"
    let f_tv = filter_with(vec![FilterAtom {
        priority: 80,
        rule: AtomRule::Source("tv".into()),
        exclude: false,
    }]);
    assert_eq!(filter::admit(vec![t_hdtv.clone()], &f_tv).admitted.len(), 1);
}
