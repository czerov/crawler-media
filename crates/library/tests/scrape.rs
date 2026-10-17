use std::fs;

use domain::{Media, MediaId, MediaKind};
use library::scrape_beside;

fn movie() -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: Some(1999),
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
fn scrape_writes_nfo_and_artwork_beside_file() {
    let tmp = tempfile::tempdir().unwrap();
    let video = tmp.path().join("The Matrix (1999).mkv");
    fs::write(&video, b"video").unwrap();
    scrape_beside(&video, &movie(), true, Some(b"\xff\xd8\xff\xdb")).unwrap();
    let nfo = tmp.path().join("The Matrix (1999).nfo");
    let poster = tmp.path().join("poster.jpg");
    assert!(nfo.is_file());
    let body = fs::read_to_string(nfo).unwrap();
    assert!(body.contains("<title>The Matrix</title>"));
    assert!(body.contains("<year>1999</year>"));
    assert!(body.contains("<tmdbid>603</tmdbid>"));
    assert_eq!(fs::read(poster).unwrap(), b"\xff\xd8\xff\xdb");
    assert_eq!(fs::read(&video).unwrap(), b"video");
}

#[test]
fn scrape_off_does_not_write_sidecars() {
    let tmp = tempfile::tempdir().unwrap();
    let video = tmp.path().join("movie.mkv");
    fs::write(&video, b"video").unwrap();
    scrape_beside(&video, &movie(), false, None).unwrap();
    assert!(!tmp.path().join("movie.nfo").exists());
    assert!(!tmp.path().join("poster.jpg").exists());
}

#[test]
fn scrape_without_poster_bytes_skips_fake_jpeg() {
    let tmp = tempfile::tempdir().unwrap();
    let video = tmp.path().join("movie.mkv");
    fs::write(&video, b"video").unwrap();
    scrape_beside(&video, &movie(), true, None).unwrap();
    let body = fs::read_to_string(tmp.path().join("movie.nfo")).unwrap();
    assert!(body.contains("<year>1999</year>"));
    assert!(!tmp.path().join("poster.jpg").exists());
}

#[test]
fn nfo_lists_present_aliases_and_omits_empty_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let video = tmp.path().join("movie.mkv");
    fs::write(&video, b"video").unwrap();
    let mut douban = movie();
    douban.tmdb_id = None;
    douban.douban_id = Some("1291843".into());
    scrape_beside(&video, &douban, true, None).unwrap();
    let body = fs::read_to_string(tmp.path().join("movie.nfo")).unwrap();
    assert!(body.contains("<doubanid>1291843</doubanid>"), "{body}");
    assert!(!body.contains("tmdbid"), "{body}");
}

#[test]
fn manual_directory_scrape_does_not_move_video() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("existing");
    fs::create_dir_all(&dir).unwrap();
    let video = dir.join("show.mkv");
    fs::write(&video, b"keep-me").unwrap();
    library::scrape_directory(&dir, &movie()).unwrap();
    assert_eq!(fs::read(&video).unwrap(), b"keep-me");
    assert!(dir.join("show.nfo").is_file());
    assert!(!dir.join("poster.jpg").exists());
}
