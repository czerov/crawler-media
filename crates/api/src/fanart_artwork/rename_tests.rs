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
