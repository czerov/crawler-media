use domain::{
    Coverage, FetchMode, FilterId, Media, MediaId, MediaKind, Subscribe, SubscribeId, UserId,
};
use std::path::Path;

fn test_media(kind: MediaKind, title: &str, year: u16) -> Media {
    Media {
        id: MediaId::new(),
        kind,
        title: title.into(),
        year: Some(year),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        bangumi_id: None,
        anilist_id: None,
        tvdb_id: None,
    }
}

fn test_subscribe(media_id: MediaId) -> Subscribe {
    Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
        keep_old_versions: false,
    }
}

#[test]
fn foreign_movies_and_mismatched_tv_are_excluded_from_transfer() {
    let media = test_media(MediaKind::Movie, "Inception", 2010);
    let sub = test_subscribe(media.id);
    let torrent_rel = release::parse("Inception.2010.1080p.BluRay.x264");

    // 1. 混入的另一部电影（年份冲突或片名冲突）必须被排除
    let foreign_file = Path::new("Inception/Another.Movie.2015.mkv");
    let foreign_rel = release::parse("Another.Movie.2015");
    let res = subscribe::file_identity::resolve_file_release(
        &sub,
        &media,
        foreign_file,
        &foreign_rel,
        &torrent_rel,
        false,
    );
    assert!(res.is_none(), "夹带的其他无关电影绝不可作为当前电影入库");

    let foreign_same_year = Path::new("Inception/Another.Movie.2010.mkv");
    let foreign_same_year_rel = release::parse("Another.Movie.2010");
    let res2 = subscribe::file_identity::resolve_file_release(
        &sub,
        &media,
        foreign_same_year,
        &foreign_same_year_rel,
        &torrent_rel,
        false,
    );
    assert!(res2.is_none(), "年份相同但片名冲突的电影必须被排除");

    // 2. 正片文件能够正常通过
    let main_file = Path::new("Inception/Inception.2010.1080p.mkv");
    let main_rel = release::parse("Inception.2010.1080p");
    let res = subscribe::file_identity::resolve_file_release(
        &sub,
        &media,
        main_file,
        &main_rel,
        &torrent_rel,
        false,
    );
    assert!(res.is_some(), "正片文件必须正常通过身份验证");
}

#[test]
fn matches_original_title_for_multi_video_files() {
    let mut media = test_media(MediaKind::Movie, "黑客帝国", 1999);
    media.original_title = Some("The Matrix".into());
    let sub = test_subscribe(media.id);
    let torrent_rel = release::parse("The.Matrix.1999.1080p.mkv");

    let file_path = Path::new("The.Matrix.1999.1080p.mkv");
    let file_rel = release::parse("The.Matrix.1999.1080p.mkv");
    let res = subscribe::file_identity::resolve_file_release(
        &sub,
        &media,
        file_path,
        &file_rel,
        &torrent_rel,
        false,
    );
    assert!(
        res.is_some(),
        "英文 original_title 应当被识别匹配，而不是误当成外来文件拒绝"
    );

    // 年份冲突依然拒绝
    let bad_year_file = Path::new("The.Matrix.2003.1080p.mkv");
    let bad_year_rel = release::parse("The.Matrix.2003.1080p.mkv");
    let res_bad = subscribe::file_identity::resolve_file_release(
        &sub,
        &media,
        bad_year_file,
        &bad_year_rel,
        &torrent_rel,
        false,
    );
    assert!(res_bad.is_none(), "年份冲突的文件绝不可入库");
}

#[test]
fn multi_file_season_pack_does_not_inherit_all_slots_without_episode() {
    let media = test_media(MediaKind::Tv, "Test Show", 2024);
    let mut sub = test_subscribe(media.id);
    sub.coverage = Coverage::Tv {
        season: 1,
        episode_from: 1,
        episode_to: Some(2),
    };
    sub.full_season_pack = true;
    let torrent_rel = release::parse("Test.Show.S01.1080p");
    let file_rel = release::parse("01.1080p.mkv");
    let resolved = subscribe::file_identity::resolve_file_release(
        &sub,
        &media,
        Path::new("Test.Show.S01.1080p/01.1080p.mkv"),
        &file_rel,
        &torrent_rel,
        false,
    );
    assert!(
        resolved.is_none(),
        "多文件整季包中无集号文件绝不能继承全部 coverage slots"
    );
}

#[test]
fn season_two_pack_with_e01_file_inherits_torrent_season_two() {
    let media = test_media(MediaKind::Tv, "Test Show", 2024);
    let mut sub = test_subscribe(media.id);
    sub.coverage = Coverage::Tv {
        season: 2,
        episode_from: 1,
        episode_to: Some(10),
    };
    sub.full_season_pack = true;
    let torrent_rel = release::parse("Test.Show.S02.1080p");
    let file_rel = release::parse("E01.1080p.mkv");
    let resolved = subscribe::file_identity::resolve_file_release(
        &sub,
        &media,
        Path::new("Test.Show.S02.1080p/E01.1080p.mkv"),
        &file_rel,
        &torrent_rel,
        false,
    );
    assert!(
        resolved.is_some(),
        "S02 季包内的 E01 文件绝不能被误判为 S01 而丢弃"
    );
    let resolved = resolved.unwrap();
    assert_eq!(resolved.season, Some(2), "季号应正确继承种子的 S02");
    assert_eq!(resolved.episode, Some(1), "集号应为 E01");
}
