use std::path::PathBuf;
use media::fanart::FanartSet;

struct RenameDuringFetch {
    old: PathBuf,
    new: PathBuf,
}

impl crate::poster_fetch::PosterFetch for RenameDuringFetch {
    fn get(&self, _: &str) -> Result<Vec<u8>, String> {
        if self.old.exists() {
            std::fs::rename(&self.old, &self.new).unwrap();
        }
        Ok(b"image".to_vec())
    }
}

#[test]
fn rename_grace_still_download_does_not_recreate_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let old = tmp.path().join("old");
    let new = tmp.path().join("new");
    std::fs::create_dir(&old).unwrap();
    let video = old.join("S01E01.strm");
    std::fs::write(&video, "https://example.test/video.mkv").unwrap();
    let fetch = RenameDuringFetch {
        old: old.clone(),
        new: new.clone(),
    };
    assert!(!super::mirror_episode_still(
        &fetch,
        &video,
        Some("/ep.jpg"),
        "w300"
    ));
    assert!(!old.exists(), "old directory must not be recreated");
    assert!(new.join("S01E01.strm").is_file());
}

#[test]
fn rename_grace_fanart_download_does_not_recreate_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let old = tmp.path().join("old");
    let new = tmp.path().join("new");
    std::fs::create_dir(&old).unwrap();
    let video = old.join("S01E01.strm");
    std::fs::write(&video, "https://example.test/video.mkv").unwrap();
    let row = super::tests::ledger_row(video.to_str().unwrap(), 1);

    let fetch = RenameDuringFetch {
        old: old.clone(),
        new: new.clone(),
    };
    let set = FanartSet {
        logo: Some(super::tests::image("https://example.test/logo.png")),
        banner: None,
        thumb: None,
        season_posters: Vec::new(),
        season_thumbs: Vec::new(),
        season_banners: Vec::new(),
    };

    let written = super::save_fanart_files(&fetch, &old, &[row], &set);
    assert_eq!(written, 0, "no files should be written to renamed away root");
    assert!(!old.exists(), "old root must not be recreated by fanart");
}

#[test]
fn movie_fanart_writes_when_movie_root_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let movie_root = tmp.path().join("Movie (2023)");
    std::fs::create_dir(&movie_root).unwrap();

    struct SimpleFetch;
    impl crate::poster_fetch::PosterFetch for SimpleFetch {
        fn get(&self, _: &str) -> Result<Vec<u8>, String> {
            Ok(b"movie_logo".to_vec())
        }
    }

    let set = FanartSet {
        logo: Some(super::tests::image("https://example.test/logo.png")),
        banner: None,
        thumb: None,
        season_posters: Vec::new(),
        season_thumbs: Vec::new(),
        season_banners: Vec::new(),
    };

    let written = super::save_fanart_files(&SimpleFetch, &movie_root, &[], &set);
    assert!(written >= 1, "movie fanart must write when movie root exists, got {written}");
    assert!(movie_root.join("logo.png").is_file());
    assert_eq!(std::fs::read(movie_root.join("logo.png")).unwrap(), b"movie_logo");
}

#[test]
fn season_poster_checks_its_own_season_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let show = tmp.path().join("Show");
    let season2 = show.join("Season 2");
    std::fs::create_dir_all(&season2).unwrap();
    let s2_video = season2.join("S02E01.strm");
    std::fs::write(&s2_video, b"stream").unwrap();

    // S1 video is missing (old root was deleted)
    let s1_missing = show.join("Season 1/S01E01.strm");

    let s1_row = super::tests::ledger_row(s1_missing.to_str().unwrap(), 1);
    let s2_row = super::tests::ledger_row(s2_video.to_str().unwrap(), 2);

    struct SimpleFetch;
    impl crate::poster_fetch::PosterFetch for SimpleFetch {
        fn get(&self, _: &str) -> Result<Vec<u8>, String> {
            Ok(b"poster_data".to_vec())
        }
    }

    fn season_image(url: &str, season: u32) -> media::fanart::FanartImage {
        media::fanart::FanartImage {
            url: url.to_string(),
            lang: None,
            likes: 0,
            season: Some(season),
        }
    }

    let set = FanartSet {
        logo: None,
        banner: None,
        thumb: None,
        season_posters: vec![season_image("https://example.test/s1.jpg", 1), season_image("https://example.test/s2.jpg", 2)],
        season_thumbs: Vec::new(),
        season_banners: Vec::new(),
    };

    let written = super::save_fanart_files(&SimpleFetch, &show, &[s1_row, s2_row], &set);
    // Only season 2 should be written
    assert!(!show.join("season01-poster.jpg").exists(), "season 1 poster should NOT be written");
    assert!(show.join("season02-poster.jpg").is_file(), "season 2 poster should be written");
    assert_eq!(written, 2); // show root season02-poster.jpg + season2/poster.jpg
}

#[test]
fn season_poster_rename_during_fetch_does_not_write_vanished_season() {
    let tmp = tempfile::tempdir().unwrap();
    let show = tmp.path().join("Show");
    let season1 = show.join("Season 1");
    let season2 = show.join("Season 2");
    std::fs::create_dir_all(&season1).unwrap();
    std::fs::create_dir_all(&season2).unwrap();
    let s1 = season1.join("S01E01.strm");
    let s2 = season2.join("S02E01.strm");
    std::fs::write(&s1, b"one").unwrap();
    std::fs::write(&s2, b"two").unwrap();
    let s1_row = super::tests::ledger_row(s1.to_str().unwrap(), 1);
    let s2_row = super::tests::ledger_row(s2.to_str().unwrap(), 2);

    struct MoveSeason {
        season: PathBuf,
    }
    impl crate::poster_fetch::PosterFetch for MoveSeason {
        fn get(&self, url: &str) -> Result<Vec<u8>, String> {
            if url.contains("/s1.jpg") {
                std::fs::remove_dir_all(&self.season).unwrap();
            }
            Ok(b"poster".to_vec())
        }
    }
    fn season_image(url: &str, season: u32) -> media::fanart::FanartImage {
        media::fanart::FanartImage {
            url: url.to_string(),
            lang: None,
            likes: 0,
            season: Some(season),
        }
    }
    let set = FanartSet {
        logo: None,
        banner: None,
        thumb: None,
        season_posters: vec![
            season_image("https://example.test/s1.jpg", 1),
            season_image("https://example.test/s2.jpg", 2),
        ],
        season_thumbs: Vec::new(),
        season_banners: Vec::new(),
    };
    let written = super::save_fanart_files(
        &MoveSeason { season: season1.clone() },
        &show,
        &[s1_row, s2_row],
        &set,
    );
    assert!(!season1.exists(), "vanished season directory must stay gone");
    assert!(!show.join("season01-poster.jpg").exists());
    assert!(show.join("season02-poster.jpg").is_file());
    assert!(season2.join("poster.jpg").is_file());
    assert_eq!(written, 2);
}
