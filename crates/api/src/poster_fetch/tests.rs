use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

struct PosterCatalog;
impl crate::catalog::Catalog for PosterCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, _: domain::MediaKind, _: &str) -> Result<Option<domain::Media>, String> {
        Ok(None)
    }
    fn poster_url(&self, _: domain::MediaKind, _: &str) -> Result<Option<String>, String> {
        Ok(Some("https://example.test/poster.jpg".into()))
    }
}

struct RenameFetch {
    video: std::path::PathBuf,
}
impl PosterFetch for RenameFetch {
    fn get(&self, _: &str) -> Result<Vec<u8>, String> {
        let _ = std::fs::remove_file(&self.video);
        Err("poster download failed after rename".into())
    }
}

struct EmptyFetcher;
impl indexer::Fetcher for EmptyFetcher {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch("unused".into()))
    }
}

#[test]
fn failed_poster_download_does_not_frame_a_vanished_video() {
    let tmp = tempfile::tempdir().unwrap();
    let video = tmp.path().join("Movie.mkv");
    std::fs::write(&video, b"video").unwrap();
    let called = AtomicBool::new(false);
    let state = ApiState::new(
        crate::Store::open(tmp.path().join("data")).unwrap(),
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
        tmp.path().join("library"),
    )
    .unwrap()
    .with_catalog(Arc::new(PosterCatalog))
    .with_poster_fetch(Arc::new(RenameFetch {
        video: video.clone(),
    }));
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "Movie".into(),
        year: Some(2023),
        original_title: None,
        tmdb_id: Some("1".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let written = attach_poster_with(&state, &media, &video, true, |_| {
        called.store(true, Ordering::SeqCst);
        panic!("ffmpeg must not run after the poster source disappears");
    });
    assert_eq!(written, ArtworkOutcome::Skipped);
    assert!(!called.load(Ordering::SeqCst));
    assert!(!video.exists());
    assert!(!tmp.path().join("poster.jpg").exists());
}

struct NoCatalog;
impl crate::catalog::Catalog for NoCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> { Ok(Vec::new()) }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> { Ok(Vec::new()) }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> { Ok(Vec::new()) }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> { Ok(Vec::new()) }
    fn details(&self, _: domain::MediaKind, _: &str) -> Result<Option<domain::Media>, String> { Ok(None) }
}

fn test_state(tmp: &std::path::Path) -> ApiState {
    ApiState::new(
        crate::Store::open(tmp.join("data")).unwrap(),
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(tmp.join("stage"))),
        tmp.join("library"),
    )
    .unwrap()
    .with_catalog(Arc::new(NoCatalog))
}

fn test_media() -> domain::Media {
    domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "Test Movie".into(),
        year: Some(2024),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

#[test]
fn frame_non_empty_but_write_fails_returns_failed() {
    let tmp = tempfile::tempdir().unwrap();
    let video = tmp.path().join("Movie.mp4");
    std::fs::write(&video, b"video-data").unwrap();
    // 把目标 poster.jpg 建成目录使 std::fs::write 必然失败
    std::fs::create_dir(tmp.path().join("poster.jpg")).unwrap();
    let outcome = attach_poster_with(&test_state(tmp.path()), &test_media(), &video, true, |_| {
        Ok(b"fake-frame-bytes".to_vec())
    });
    assert_eq!(outcome, ArtworkOutcome::Failed);
}

#[test]
fn frame_after_source_vanishes_returns_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let video = tmp.path().join("Movie.mp4");
    std::fs::write(&video, b"video-data").unwrap();
    let v_clone = video.clone();
    let outcome = attach_poster_with(&test_state(tmp.path()), &test_media(), &video, true, move |_| {
        let _ = std::fs::remove_file(&v_clone);
        Ok(b"fake-frame-bytes".to_vec())
    });
    assert_eq!(outcome, ArtworkOutcome::Skipped);
    assert!(!tmp.path().join("poster.jpg").exists());
}

#[test]
fn frame_after_cancelled_publication_returns_failed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("homevideos");
    std::fs::create_dir_all(&root).unwrap();
    let state = test_state(tmp.path());
    let library = state.store().lock().create_library(
        domain::MediaKind::Movie,
        "Movies",
        &[root.to_str().unwrap()],
        "everyone",
        true,
        &[],
    ).unwrap();
    let video = root.join("Movie.mp4");
    std::fs::write(&video, b"video-data").unwrap();
    let media = test_media();
    let target = root.join("poster.jpg");
    let called = std::sync::atomic::AtomicBool::new(false);
    let called_ref = &called;
    let state_ref = &state;
    let library_id = library.id.clone();
    let outcome = attach_poster_with(&state, &media, &video, true, move |_| {
        called_ref.store(true, std::sync::atomic::Ordering::SeqCst);
        state_ref.library_work().cancel(&library_id);
        Ok(b"fake-frame-bytes".to_vec())
    });
    assert!(called.load(std::sync::atomic::Ordering::SeqCst), "frame 闭包必须被执行");
    assert_eq!(outcome, ArtworkOutcome::Failed);
    assert!(!target.exists(), "已取消的工作不得写入海报目标文件");
}

#[test]
fn library_lease_publish_after_cancelled_control_returns_err() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("homevideos");
    std::fs::create_dir_all(&root).unwrap();
    let state = test_state(tmp.path());
    let _ = state.store().lock().create_library(
        domain::MediaKind::Movie,
        "Movies",
        &[root.to_str().unwrap()],
        "everyone",
        true,
        &[],
    ).unwrap();
    let video = root.join("Movie.mp4");
    std::fs::write(&video, b"video-data").unwrap();
    let media = test_media();
    let lease = state.acquire_path_work(&video, domain::MediaKind::Movie).unwrap().unwrap();
    lease.control().cancel();
    let outcome = lease.publish(|| Ok(write_artwork(&media, &root.join("poster.jpg"), "海报", b"fake-frame-bytes")));
    assert!(outcome.is_err(), "底层租约取消后 lease.publish 必须返回 Err");
}
