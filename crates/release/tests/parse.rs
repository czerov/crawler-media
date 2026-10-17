use release::parse;

#[test]
fn parses_subtitle_and_audio_language_markers() {
    let r = parse("Show.2024.S01E01.1080p.CHS.国语.WEB-DL-GROUP");
    assert_eq!(r.subtitle_language.as_deref(), Some("zh-Hans"));
    assert_eq!(r.audio_language.as_deref(), Some("cmn"));

    let r = parse("Show.2024.S01E01.1080p.CHT.粤语.WEB-DL-GROUP");
    assert_eq!(r.subtitle_language.as_deref(), Some("zh-Hant"));
    assert_eq!(r.audio_language.as_deref(), Some("yue"));

    let r = parse("Movie.2024.1080p.WEB-DL-GROUP");
    assert_eq!(r.subtitle_language, None, "无语言标记时应保持 None");
    assert_eq!(r.audio_language, None);
}

#[test]
fn zh_marker_matches_either_simplified_or_traditional() {
    let r = parse("Show.2024.S01E01.1080p.CHS&CHT.WEB-DL-GROUP");
    assert_eq!(
        r.subtitle_language.as_deref(),
        Some("zh"),
        "简繁双字归一为 zh"
    );

    let r = parse("Show.2024.S01E01.1080p.简繁中字.WEB-DL-GROUP");
    assert_eq!(r.subtitle_language.as_deref(), Some("zh"));
}

#[test]
fn parses_chinese_episode_and_dash_s_e_format() {
    // 经典网盘转存/strm 格式
    let r = parse("一瓯春 - S01E05 - 第 5 集.strm");
    assert_eq!(r.title, "一瓯春");
    assert_eq!(r.season, Some(1));
    assert_eq!(r.episode, Some(5));
    assert_eq!(r.confidence, domain::Confidence::High);

    let r = parse("【官方中字】狂飙 - S01E12.mp4");
    assert_eq!(r.title, "狂飙");
    assert_eq!(r.season, Some(1));
    assert_eq!(r.episode, Some(12));
    assert_eq!(r.confidence, domain::Confidence::High);
    assert_eq!(
        r.subtitle_language.as_deref(),
        Some("zh"),
        "前置【官方中字】标签也必须正确提取语言"
    );

    let r = parse("繁花 - 第 08 集.mkv");
    assert_eq!(r.title, "繁花");
    assert_eq!(r.season, Some(1));
    assert_eq!(r.episode, Some(8));
    assert_eq!(r.confidence, domain::Confidence::High);
}

#[test]
fn remux_takes_precedence_over_bluray() {
    let r1 = parse("Arrival.2016.1080p.BluRay.REMUX.AVC.DTS-HD.MA.7.1-FGT");
    assert_eq!(r1.source.as_deref(), Some("Remux"));

    let r2 = parse("Arrival.2016.1080p.REMUX.BluRay.AVC-GROUP");
    assert_eq!(r2.source.as_deref(), Some("Remux"));

    let r3 = parse("Arrival.2016.1080p.BluRay.x264-GROUP");
    assert_eq!(r3.source.as_deref(), Some("BluRay"));
}

#[test]
fn boundary_episode_range_and_title_are_normalized() {
    let r = parse("The.Long.Watch.2024.S01E03-E04.1080p.WEB-DL");
    assert_eq!(r.title, "The Long Watch");
    assert_eq!(r.year, Some(2024));
    assert_eq!(r.season, Some(1));
    assert_eq!(r.episode, Some(3));
    assert_eq!(r.episode_to, Some(4));
}

#[test]
fn huge_parsed_episode_range_is_capped_when_expanded() {
    let r = parse("Test.Show.S01E01-E4294967295.1080p");
    assert_eq!(r.season, Some(1));
    assert_eq!(r.episode, Some(1));
    assert_eq!(r.episode_to, Some(4_294_967_295));
    let units = r.covered_episodes();
    assert_eq!(units.len(), domain::Coverage::MAX_EPISODES as usize);
    assert_eq!(units[0], (1, 1));
    assert_eq!(units[units.len() - 1], (1, domain::Coverage::MAX_EPISODES));
}

#[test]
fn parses_pterclub_space_separated_season_and_ep_range() {
    // 用户的实际错误样例 1
    let r =
        parse("Once upon a Time in Longfan 2026 S01 E01-E04 2160p WEB-DL H.264 AAC 2.0-PTerWEB");
    assert_eq!(r.title, "Once upon a Time in Longfan");
    assert_eq!(r.year, Some(2026));
    assert_eq!(r.season, Some(1));
    assert_eq!(r.episode, Some(1));
    assert_eq!(r.episode_to, Some(4));
    assert_eq!(r.resolution.as_deref(), Some("2160p"));
    assert_eq!(r.codec.as_deref(), Some("H.264"));
    assert_eq!(r.source.as_deref(), Some("WEB-DL"));
    assert_eq!(r.confidence, domain::Confidence::High);

    // 用户的实际错误样例 2
    let r = parse("Once upon a Time in Longfan 2026 S01 E05-E06 1080p WEB-DL H264 AAC-PTerWEB");
    assert_eq!(r.title, "Once upon a Time in Longfan");
    assert_eq!(r.year, Some(2026));
    assert_eq!(r.season, Some(1));
    assert_eq!(r.episode, Some(5));
    assert_eq!(r.episode_to, Some(6));
    assert_eq!(r.resolution.as_deref(), Some("1080p"));
    assert_eq!(r.codec.as_deref(), Some("H264"));
}

#[test]
fn parses_complete_season_pack_without_episodes() {
    let r = parse("Cherry Season S01 2014 2160p 60fps WEB-DL H265 AAC-XXX");
    assert_eq!(r.title, "Cherry Season");
    assert_eq!(r.season, Some(1));
    assert_eq!(r.episode, None);
    assert_eq!(r.year, Some(2014));
    assert_eq!(r.resolution.as_deref(), Some("2160p"));

    let r = parse("24 S01 1080p WEB-DL AAC2.0 H.264-BTN");
    assert_eq!(r.title, "24");
    assert_eq!(r.season, Some(1));
    assert_eq!(r.episode, None);
    assert_eq!(r.resolution.as_deref(), Some("1080p"));
}

#[test]
fn parses_standalone_episodes_and_ranges() {
    let r = parse("The Heart of Genius S01 13-14 2022 1080p WEB-DL H264 AAC");
    assert_eq!(r.title, "The Heart of Genius");
    assert_eq!(r.season, Some(1));
    assert_eq!(r.episode, Some(13));
    assert_eq!(r.episode_to, Some(14));

    let r = parse("Show.Name.E05.1080p.WEB-DL");
    assert_eq!(r.title, "Show Name");
    assert_eq!(r.season, Some(1));
    assert_eq!(r.episode, Some(5));
}

#[test]
fn parses_chinese_seasons_and_ranges() {
    let r = parse("大理寺少卿游 第二季 第01-04集 4k WEB-DL");
    assert_eq!(r.title, "大理寺少卿游");
    assert_eq!(r.season, Some(2));
    assert_eq!(r.episode, Some(1));
    assert_eq!(r.episode_to, Some(4));
    assert_eq!(r.resolution.as_deref(), Some("2160p"));
}

#[test]
fn season_followed_by_resolution_is_not_parsed_as_episode() {
    let r = parse("Breaking.Bad.S02.720p.HDTV.x264");
    assert_eq!(r.title, "Breaking Bad");
    assert_eq!(r.season, Some(2));
    assert_eq!(r.episode, None);
    assert_eq!(r.resolution.as_deref(), Some("720p"));

    let r2 = parse("The.Wire.S04.480p.DVDRip.x264");
    assert_eq!(r2.title, "The Wire");
    assert_eq!(r2.season, Some(4));
    assert_eq!(r2.episode, None);
    assert_eq!(r2.resolution.as_deref(), Some("480p"));
}

#[test]
fn calendar_dates_are_not_episodes() {
    for raw in [
        "Chinese Dream Program 20261002 1080p",
        "Chinese.Dream.Family.20261002.1080p.WEB-DL",
        "Chinese Dream 2026.10.02 1080p",
        "Show.Name.E20261002.1080p",
        "Show.Name.EP20261002.1080p",
    ] {
        let release = parse(raw);
        assert_eq!(release.episode, None, "{raw}");
        assert_eq!(release.episode_to, None, "{raw}");
    }
}

#[test]
fn ordinary_episode_tokens_still_parse() {
    let dotted = parse("Show.Name.20261002.E01.1080p");
    assert_eq!((dotted.season, dotted.episode), (Some(1), Some(1)));
    let range = parse("The.Long.Watch.2024.S01E03-E04.1080p.WEB-DL");
    assert_eq!(
        (range.season, range.episode, range.episode_to),
        (Some(1), Some(3), Some(4))
    );
}
