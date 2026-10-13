use std::path::PathBuf;

use domain::{Media, MediaKind};
use library::{WatchJob, WatchKind, is_video_file, scan_watch_reported};

#[test]
fn scan_watch_reported_accurately_records_issues() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();

    let movie = root.join("one (2024).mkv");
    std::fs::write(&movie, b"video data").unwrap();

    let job = WatchJob {
        kind: WatchKind::InPlace,
        path: root.clone(),
        library_root: root.clone(),
        tv_library_root: None,
        scrape: false,
    };
    let media = Media {
        id: domain::MediaId::new(),
        kind: MediaKind::Movie,
        title: "Test".into(),
        year: None,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };

    let (outcome, report) = scan_watch_reported(&job, &media).unwrap();
    assert_eq!(outcome.transferred.len(), 1);
    assert_eq!(report.visited, 1);
    assert_eq!(report.error_count, 0);
    assert_eq!(report.status, library::ScanRootStatus::Completed);
}

#[test]
fn unreachable_root_reports_unreachable() {
    let tmp = tempfile::tempdir().unwrap();
    let missing_root = tmp.path().join("non_existent");

    let job = WatchJob {
        kind: WatchKind::InPlace,
        path: missing_root.clone(),
        library_root: missing_root.clone(),
        tv_library_root: None,
        scrape: false,
    };
    let media = Media {
        id: domain::MediaId::new(),
        kind: MediaKind::Movie,
        title: "Test".into(),
        year: None,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };

    let (outcome, report) = scan_watch_reported(&job, &media).unwrap();
    assert_eq!(outcome.transferred.len(), 0);
    assert_eq!(report.status, library::ScanRootStatus::Unreachable);
}

#[test]
fn is_video_file_recognizes_all_extended_containers() {
    assert!(is_video_file(&PathBuf::from("video.mts")));
    assert!(is_video_file(&PathBuf::from("video.vob")));
    assert!(is_video_file(&PathBuf::from("video.iso")));
    assert!(is_video_file(&PathBuf::from("video.mkv")));
    assert!(!is_video_file(&PathBuf::from("video.txt")));
}
