use std::path::Path;
use std::sync::{Arc, Mutex};

use api::catalog::{ArtworkCandidate, ArtworkCandidates, Catalog};
use api::{ApiState, PosterFetch, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media::CatalogHit;
use serde_json::Value;
use tower::ServiceExt;

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

#[derive(Clone)]
struct ArtworkCatalog {
    media: Media,
}

impl Catalog for ArtworkCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }

    fn search_tv(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }

    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }

    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }

    fn details(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Option<Media>, String> {
        Ok(Some(self.media.clone()))
    }

    fn poster_url(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Option<String>, String> {
        Ok(Some(
            "https://image.tmdb.org/t/p/w342/pantheon-poster.jpg".into(),
        ))
    }

    fn image_candidates(
        &self,
        _kind: MediaKind,
        _tmdb_id: &str,
    ) -> Result<ArtworkCandidates, String> {
        Ok(ArtworkCandidates {
            posters: vec![ArtworkCandidate {
                file_path: "/pantheon-poster.jpg".into(),
                width: 2000,
                height: 3000,
                language: None,
            }],
            backdrops: vec![ArtworkCandidate {
                file_path: "/pantheon-backdrop.jpg".into(),
                width: 3840,
                height: 2160,
                language: None,
            }],
        })
    }
}

#[derive(Default)]
struct CapturingPosterFetch {
    urls: Mutex<Vec<String>>,
}

impl PosterFetch for CapturingPosterFetch {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        self.urls.lock().unwrap().push(url.to_string());
        Ok(b"poster-image-bytes".to_vec())
    }
}

fn media(kind: MediaKind, title: &str, tmdb_id: &str) -> Media {
    Media {
        id: MediaId::new(),
        kind,
        title: title.into(),
        year: Some(2022),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some(tmdb_id.into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn app_state(root: &Path, store: Store) -> ApiState {
    ApiState::new(
        store,
        "admin-token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
}

async fn response_json(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

#[tokio::test]
async fn unified_tv_detail_uses_large_poster_and_backdrop_urls() {
    let tmp = tempfile::tempdir().unwrap();
    let show = media(MediaKind::Tv, "Pantheon", "195339");
    let store = Store::open(tmp.path().join("data")).unwrap();
    let state = app_state(tmp.path(), store).with_catalog(Arc::new(ArtworkCatalog { media: show }));
    let app = router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/media/tv/195339")
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(
        body["data"]["item"]["poster_url"],
        "https://image.tmdb.org/t/p/w780/pantheon-poster.jpg"
    );
    assert_eq!(
        body["data"]["item"]["backdrop_url"],
        "https://image.tmdb.org/t/p/w1280/pantheon-backdrop.jpg"
    );
}

#[tokio::test]
async fn metadata_refresh_fetches_the_configured_large_default_poster() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let library_root = tmp.path().join("library/movies");
    std::fs::create_dir_all(&library_root).unwrap();
    let library_root_string = library_root.display().to_string();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[library_root_string.as_str()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let film = media(MediaKind::Movie, "Film", "603");
    store.insert_media(&film).unwrap();
    let video = library_root.join("Film.strm");
    std::fs::write(&video, "https://media.example/film").unwrap();
    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: film.id,
            path: video.display().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();

    let fetcher = Arc::new(CapturingPosterFetch::default());
    let state = app_state(tmp.path(), store)
        .with_catalog(Arc::new(ArtworkCatalog { media: film }))
        .with_poster_fetch(fetcher.clone());
    let app = router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/libraries/{}/metadata/refresh", library.id))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        fetcher.urls.lock().unwrap().first().map(String::as_str),
        Some("https://image.tmdb.org/t/p/w780/pantheon-poster.jpg")
    );
    assert_eq!(
        std::fs::read(library_root.join("poster.jpg")).unwrap(),
        b"poster-image-bytes"
    );
}
