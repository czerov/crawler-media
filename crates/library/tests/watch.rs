use std::fs;

use domain::{Confidence, Media, MediaId, MediaKind};
use library::{WatchJob, WatchKind, scan_watch};

fn movie() -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

#[test]
fn intake_transfers_high_confidence_file_into_library() {
    let tmp = tempfile::tempdir().unwrap();
    let intake = tmp.path().join("intake");
    let library = tmp.path().join("library");
    fs::create_dir_all(&intake).unwrap();
    fs::create_dir_all(&library).unwrap();
    let src = intake.join("The.Matrix.1999.2160p.BluRay.mkv");
    fs::write(&src, b"video").unwrap();
    let job = WatchJob {
        kind: WatchKind::Intake,
        scrape: true,
        path: intake.clone(),
        library_root: library.clone(),
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &movie()).unwrap();
    assert_eq!(outcome.transferred.len(), 1);
    assert!(outcome.unidentified.is_empty());
    assert!(library.join("The.Matrix.1999.2160p.BluRay.mkv").is_file());
}

#[test]
fn intake_does_not_overwrite_an_existing_library_file() {
    let tmp = tempfile::tempdir().unwrap();
    let intake = tmp.path().join("intake");
    let library = tmp.path().join("library");
    fs::create_dir_all(&intake).unwrap();
    fs::create_dir_all(&library).unwrap();
    let dest = library.join("The.Matrix.1999.2160p.BluRay.mkv");
    fs::write(&dest, b"old-library").unwrap();
    let src = intake.join("The.Matrix.1999.2160p.BluRay.mkv");
    fs::write(&src, b"new-drop").unwrap();
    let job = WatchJob {
        kind: WatchKind::Intake,
        scrape: false,
        path: intake,
        library_root: library,
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &movie()).unwrap();
    assert!(outcome.transferred.is_empty());
    assert_eq!(outcome.errors.len(), 1);
    assert!(outcome.errors[0].error.contains("already exists"));
    assert_eq!(outcome.errors[0].path, src);
    assert_eq!(fs::read(&dest).unwrap(), b"old-library");
}

#[test]
fn intake_batch_partial_success_does_not_discard_successful_transfers() {
    let tmp = tempfile::tempdir().unwrap();
    let intake = tmp.path().join("intake");
    let library = tmp.path().join("library");
    fs::create_dir_all(&intake).unwrap();
    fs::create_dir_all(&library).unwrap();

    // A: 正常文件
    let src_a = intake.join("The.Matrix.1999.2160p.BluRay.mkv");
    fs::write(&src_a, b"matrix-bytes").unwrap();

    // B: 冲突文件（library 中已有不同内容）
    let dest_b = library.join("Inception.2010.1080p.BluRay.mkv");
    fs::write(&dest_b, b"inception-existing").unwrap();
    let src_b = intake.join("Inception.2010.1080p.BluRay.mkv");
    fs::write(&src_b, b"inception-new").unwrap();

    // C: 正常文件
    let src_c = intake.join("Interstellar.2014.2160p.BluRay.mkv");
    fs::write(&src_c, b"interstellar-bytes").unwrap();

    let job = WatchJob {
        kind: WatchKind::Intake,
        scrape: false,
        path: intake,
        library_root: library.clone(),
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &movie()).unwrap();

    // 冲突项 B 记录在 errors 中，旧字节未被覆盖
    assert_eq!(outcome.errors.len(), 1);
    assert_eq!(outcome.errors[0].path, src_b);
    assert!(outcome.errors[0].error.contains("already exists"));
    assert_eq!(fs::read(&dest_b).unwrap(), b"inception-existing");

    // 正常项 A 和 C 成功转移
    let transferred_paths: Vec<_> = outcome.transferred.iter().map(|t| &t.path).collect();
    let dest_a = library.join("The.Matrix.1999.2160p.BluRay.mkv");
    let dest_c = library.join("Interstellar.2014.2160p.BluRay.mkv");
    assert!(transferred_paths.contains(&&dest_a));
    assert!(transferred_paths.contains(&&dest_c));
    assert_eq!(fs::read(&dest_a).unwrap(), b"matrix-bytes");
    assert_eq!(fs::read(&dest_c).unwrap(), b"interstellar-bytes");
}

#[test]
fn in_place_watch_keeps_unidentified_video_without_placeholder_nfo() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lib");
    fs::create_dir_all(&dir).unwrap();
    let video = dir.join("The.Matrix.1999.2160p.mkv");
    fs::write(&video, b"stay").unwrap();
    let job = WatchJob {
        kind: WatchKind::InPlace,
        scrape: true,
        path: dir.clone(),
        library_root: dir.clone(),
        tv_library_root: None,
    };
    scan_watch(&job, &movie()).unwrap();
    assert_eq!(fs::read(&video).unwrap(), b"stay");
    assert!(!dir.join("The.Matrix.1999.2160p.nfo").exists());
}

#[test]
fn in_place_watch_scrapes_nested_season_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lib");
    let nested = dir.join("The Long Watch/Season 01");
    fs::create_dir_all(&nested).unwrap();
    let video = nested.join("The.Long.Watch.S01E01.1080p.mkv");
    fs::write(&video, b"stay").unwrap();
    let job = WatchJob {
        kind: WatchKind::InPlace,
        scrape: true,
        path: dir.clone(),
        library_root: dir.clone(),
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &movie()).unwrap();
    assert_eq!(fs::read(&video).unwrap(), b"stay");
    assert!(!nested.join("The.Long.Watch.S01E01.1080p.nfo").exists());
    assert_eq!(outcome.transferred, vec![video]);
}

#[test]
fn low_confidence_intake_is_unidentified() {
    let tmp = tempfile::tempdir().unwrap();
    let intake = tmp.path().join("intake");
    let library = tmp.path().join("library");
    fs::create_dir_all(&intake).unwrap();
    fs::create_dir_all(&library).unwrap();
    let src = intake.join("foo.mkv");
    fs::write(&src, b"??").unwrap();
    let job = WatchJob {
        kind: WatchKind::Intake,
        scrape: true,
        path: intake,
        library_root: library.clone(),
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &movie()).unwrap();
    assert_eq!(outcome.unidentified.len(), 1);
    assert_eq!(outcome.unidentified[0].confidence, Confidence::Low);
    assert!(library.read_dir().unwrap().next().is_none());
}

#[test]
fn in_place_watch_supports_avi_ts_and_other_video_formats() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lib");
    fs::create_dir_all(&dir).unwrap();
    let avi = dir.join("Movie.1999.1080p.avi");
    let ts = dir.join("Movie.1999.1080p.ts");
    fs::write(&avi, b"avi-video").unwrap();
    fs::write(&ts, b"ts-video").unwrap();
    let job = WatchJob {
        kind: WatchKind::InPlace,
        scrape: false,
        path: dir.clone(),
        library_root: dir.clone(),
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &movie()).unwrap();
    assert!(outcome.transferred.iter().any(|t| t.path == avi));
    assert!(outcome.transferred.iter().any(|t| t.path == ts));
}

#[test]
fn in_place_watch_resolves_low_confidence_movie_via_nfo_or_parent_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lib/The Matrix (1999)");
    fs::create_dir_all(&dir).unwrap();
    // 只有通用名字 movie.mkv，自身 release::parse 是 Low
    let movie_file = dir.join("movie.mkv");
    let nfo_file = dir.join("movie.nfo");
    fs::write(&movie_file, b"video-data").unwrap();
    fs::write(
        &nfo_file,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<movie>
    <title>The Matrix</title>
    <year>1999</year>
</movie>"#,
    )
    .unwrap();
    let job = WatchJob {
        kind: WatchKind::InPlace,
        scrape: false,
        path: dir.clone(),
        library_root: dir.clone(),
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &movie()).unwrap();
    assert_eq!(outcome.transferred, vec![movie_file]);
}

#[test]
fn intake_ignores_non_video_files_like_subtitles() {
    let tmp = tempfile::tempdir().unwrap();
    let intake = tmp.path().join("intake");
    let library = tmp.path().join("library");
    fs::create_dir_all(&intake).unwrap();
    fs::create_dir_all(&library).unwrap();
    let srt = intake.join("The.Matrix.1999.srt");
    fs::write(&srt, b"1\n00:00:01,000 --> 00:00:02,000\nHello").unwrap();
    let job = WatchJob {
        kind: WatchKind::Intake,
        scrape: false,
        path: intake,
        library_root: library,
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &movie()).unwrap();
    // 字幕不作为视频被转移或标记为未识别视频
    assert!(outcome.transferred.is_empty());
    assert!(outcome.unidentified.is_empty());
}

#[test]
fn watch_kind_is_two_jobs_not_one_switch() {
    assert_ne!(WatchKind::Intake, WatchKind::InPlace);
}

#[test]
fn high_confidence_episode_keeps_filename_title_and_inherits_ancestor_year() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp
        .path()
        .join("children/动灵守护者 (2022)/Spirit.Rangers.S01");
    fs::create_dir_all(&dir).unwrap();
    let episode = dir.join(
        "Spirit.Rangers.S01E01.Thunder.Mountain.1080p.NF.WEB-DL.strm",
    );
    fs::write(&episode, b"https://cdn.example/ep.mkv").unwrap();
    let job = WatchJob {
        kind: WatchKind::InPlace,
        scrape: false,
        path: tmp.path().to_path_buf(),
        library_root: tmp.path().to_path_buf(),
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &movie()).unwrap();
    let found = outcome
        .transferred
        .iter()
        .find(|file| file.path == episode)
        .unwrap();
    let parsed = found.identified_release.as_ref().unwrap();
    assert_eq!(parsed.title, "Spirit Rangers");
    assert_eq!(parsed.year, Some(2022));
    assert_eq!(parsed.season, Some(1));
    assert_eq!(parsed.episode, Some(1));
}
