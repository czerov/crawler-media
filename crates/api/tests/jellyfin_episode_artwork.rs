use std::path::Path;
use std::sync::Arc;

use api::catalog::Catalog;
use api::{ApiState, Store, router};
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

struct EpisodeCatalog;

impl Catalog for EpisodeCatalog {
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
        Ok(None)
    }

    fn episode_stills(
        &self,
        _tmdb_id: &str,
        season: u32,
        episode: u32,
    ) -> Result<Vec<String>, String> {
        Ok(vec![format!("/{season}-{episode}-still.jpg")])
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

fn episode_row(media_id: MediaId, path: &Path, episode: u32) -> LedgerRow {
    LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: path.display().to_string(),
        season: Some(1),
        episode: Some(episode),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
    }
}

#[tokio::test]
async fn jellyfin_series_primary_image_uses_series_poster_and_episode_uses_still() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let library_root = tmp.path().join("library/tv");
    let show_dir = library_root.join("Pantheon");
    let episode_dir = show_dir.join("Season 1");
    std::fs::create_dir_all(&episode_dir).unwrap();
    let library_root_string = library_root.display().to_string();
    store
        .create_library(
            MediaKind::Tv,
            "TV",
            &[library_root_string.as_str()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let show = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Pantheon".into(),
        year: Some(2022),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("195339".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&show).unwrap();
    let video = episode_dir.join("Pantheon S01E01.strm");
    std::fs::write(&video, "https://media.example/episode-1").unwrap();
    std::fs::write(show_dir.join("poster.jpg"), b"series-poster").unwrap();
    std::fs::write(
        episode_dir.join("Pantheon S01E01-still.jpg"),
        b"episode-still",
    )
    .unwrap();
    let row = episode_row(show.id, &video, 1);
    store.insert_ledger(&row).unwrap();

    let app = router(app_state(tmp.path(), store).with_catalog(Arc::new(EpisodeCatalog)));
    let series_id = show.id.to_string().replace('-', "");
    let episode_id = row.id.to_string().replace('-', "");
    let series_item = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/Items/{series_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let series_item = response_json(series_item).await;
    assert_eq!(series_item["Type"], "Series");
    let original_tag = series_item["ImageTags"]["Primary"]
        .as_str()
        .unwrap()
        .to_owned();

    for (item_id, expected) in [
        (&series_id, &b"series-poster"[..]),
        (&episode_id, &b"episode-still"[..]),
    ] {
        let image = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{item_id}/Images/Primary"))
                    .header("authorization", "Bearer admin-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(image.status(), StatusCode::OK);
        let image = to_bytes(image.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&image[..], expected);
    }

    std::fs::write(show_dir.join("poster.jpg"), b"updated-series-poster").unwrap();
    let updated_item = app
        .oneshot(
            Request::builder()
                .uri(format!("/Items/{series_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let updated_tag = response_json(updated_item).await["ImageTags"]["Primary"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(
        original_tag, updated_tag,
        "updated artwork must bust client caches"
    );
}

#[tokio::test]
async fn jellyfin_uses_unique_episode_stills_before_the_season_poster() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let library_root = tmp.path().join("library/tv");
    let episode_dir = library_root.join("Pantheon/Season 1");
    std::fs::create_dir_all(&episode_dir).unwrap();
    let library_root_string = library_root.display().to_string();
    store
        .create_library(
            MediaKind::Tv,
            "TV",
            &[library_root_string.as_str()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let show = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Pantheon".into(),
        year: Some(2022),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("195339".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&show).unwrap();
    let first_video = episode_dir.join("Pantheon S01E01.strm");
    let second_video = episode_dir.join("Pantheon S01E02.strm");
    std::fs::write(&first_video, "https://media.example/episode-1").unwrap();
    std::fs::write(&second_video, "https://media.example/episode-2").unwrap();
    std::fs::write(episode_dir.join("poster.jpg"), b"season-poster").unwrap();
    std::fs::write(
        episode_dir.join("Pantheon S01E01-still.jpg"),
        b"episode-one-still",
    )
    .unwrap();
    let first = episode_row(show.id, &first_video, 1);
    let second = episode_row(show.id, &second_video, 2);
    store.insert_ledger(&first).unwrap();
    store.insert_ledger(&second).unwrap();
    let first_id = first.id.to_string().replace('-', "");
    let second_id = second.id.to_string().replace('-', "");
    let app = router(app_state(tmp.path(), store).with_catalog(Arc::new(EpisodeCatalog)));

    let first_item = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/Items/{first_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response_json(first_item).await["ImageTags"]["Primary"],
        "episode"
    );
    let first_image = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/Items/{first_id}/Images/Primary"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_image.status(), StatusCode::OK);
    let first_image = to_bytes(first_image.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&first_image[..], b"episode-one-still");

    let second_item = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/Items/{second_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response_json(second_item).await["ImageTags"]["Primary"],
        "episode"
    );
    let second_image = app
        .oneshot(
            Request::builder()
                .uri(format!("/Items/{second_id}/Images/Primary"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_image.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        second_image.headers()[axum::http::header::LOCATION],
        "https://image.tmdb.org/t/p/w300/1-2-still.jpg"
    );
}
