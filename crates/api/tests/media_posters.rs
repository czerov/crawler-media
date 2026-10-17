use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::http::{Request, StatusCode};
use domain::{Media, MediaId, MediaKind, Torrent};
use downloader::{Downloader, DownloaderError};
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media::CatalogHit;
use tower::ServiceExt;

use api::catalog::Catalog;
use api::management::ApiState;
use api::media_posters::http::ImageFetcher;
use api::media_posters::service::MediaPosterService;
use api::media_posters::PosterError;
use api::Store;

struct MockCatalog {
    poster_calls: AtomicUsize,
}

impl Catalog for MockCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, _: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn poster_url(&self, _: MediaKind, tmdb_id: &str) -> Result<Option<String>, String> {
        self.poster_calls.fetch_add(1, Ordering::SeqCst);
        if tmdb_id == "valid" {
            Ok(Some("https://image.tmdb.org/t/p/w500/sample.jpg".into()))
        } else {
            Ok(None)
        }
    }
}

struct MockFetcher {
    fetch_calls: AtomicUsize,
}

impl ImageFetcher for MockFetcher {
    fn fetch(&self, url: &str) -> Result<(String, Vec<u8>), PosterError> {
        self.fetch_calls.fetch_add(1, Ordering::SeqCst);
        if url.contains("sample.jpg") {
            Ok(("image/png".into(), vec![0x89, 0x50, 0x4E, 0x47]))
        } else {
            Err(PosterError::NotFound)
        }
    }
}

struct NullDownloader;

impl Downloader for NullDownloader {
    fn add(&self, _: &Torrent) -> Result<(), DownloaderError> {
        Ok(())
    }
    fn completed_files(&self, _: &Torrent) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        Ok(Vec::new())
    }
}

struct NullFetcher;

impl Fetcher for NullFetcher {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Ok(String::new())
    }
}

#[tokio::test]
async fn media_poster_route_serves_public_bytes_and_caches_on_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let cache_dir = tmp.path().join("cache");
    let store = Store::open(&data_dir).unwrap();

    let media_id = MediaId::new();
    let media = Media {
        id: media_id,
        kind: MediaKind::Movie,
        title: "Matrix".into(),
        year: Some(1999),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("valid".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();

    let catalog = Arc::new(MockCatalog {
        poster_calls: AtomicUsize::new(0),
    });
    let fetcher = Arc::new(MockFetcher {
        fetch_calls: AtomicUsize::new(0),
    });

    let state = ApiState::new(
        store,
        "test-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NullFetcher),
        Arc::new(NullDownloader),
        tmp.path().join("library"),
    )
    .unwrap();

    let poster_service = Arc::new(MediaPosterService::with_custom_fetcher(
        state.store(),
        catalog.clone(),
        cache_dir,
        fetcher.clone(),
    ));

    let state = state.with_media_posters(poster_service);
    let app = api::router(state);

    // 1. 无 Authorization header 的原生 <img> 请求也能访问
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/media/{media_id}/poster"))
        .body(axum::body::Body::empty())
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["content-type"], "image/png");
    assert_eq!(res.headers()["cache-control"], "public, max-age=86400");
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    assert_eq!(&body[..], &[0x89, 0x50, 0x4E, 0x47]);

    assert_eq!(catalog.poster_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fetcher.fetch_calls.load(Ordering::SeqCst), 1);

    // 2. 第二次请求直接命中磁盘缓存，不再触发 catalog 或 fetcher
    let req2 = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/media/{media_id}/poster"))
        .body(axum::body::Body::empty())
        .unwrap();

    let res2 = app.clone().oneshot(req2).await.unwrap();
    assert_eq!(res2.status(), StatusCode::OK);
    assert_eq!(catalog.poster_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fetcher.fetch_calls.load(Ordering::SeqCst), 1);

    // 3. 不存在的 media 返回 404
    let unknown_id = MediaId::new();
    let req_unknown = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/media/{unknown_id}/poster"))
        .body(axum::body::Body::empty())
        .unwrap();
    let res_unknown = app.clone().oneshot(req_unknown).await.unwrap();
    assert_eq!(res_unknown.status(), StatusCode::NOT_FOUND);

    // 4. 非法 UUID 返回 400
    let req_bad = Request::builder()
        .method("GET")
        .uri("/api/v1/media/not-a-uuid/poster")
        .body(axum::body::Body::empty())
        .unwrap();
    let res_bad = app.oneshot(req_bad).await.unwrap();
    assert_eq!(res_bad.status(), StatusCode::BAD_REQUEST);
}
