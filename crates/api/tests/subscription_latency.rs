use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::http::{Request, StatusCode};
use domain::{Coverage, FetchMode, Media, MediaId, MediaKind, Subscribe, SubscribeId, Torrent, UserRole};
use downloader::{Downloader, DownloaderError};
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media::CatalogHit;
use serde_json::Value;
use tower::ServiceExt;

use api::catalog::Catalog;
use api::management::ApiState;
use api::Store;

struct ForbiddenPosterCatalog {
    poster_calls: AtomicUsize,
}

impl ForbiddenPosterCatalog {
    fn new() -> Self {
        Self {
            poster_calls: AtomicUsize::new(0),
        }
    }
}

impl Catalog for ForbiddenPosterCatalog {
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
    fn poster_url(&self, _: MediaKind, _: &str) -> Result<Option<String>, String> {
        self.poster_calls.fetch_add(1, Ordering::SeqCst);
        Err("list/dto must not synchronously call remote catalog poster_url".into())
    }
}

struct NullDownloader;

impl Downloader for NullDownloader {
    fn add(&self, _: &Torrent) -> Result<(), DownloaderError> {
        Ok(())
    }
    fn completed_files(&self, _: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        Ok(Vec::new())
    }
}

struct NullFetcher;

impl Fetcher for NullFetcher {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Ok(String::new())
    }
}

fn authed_get(uri: &str) -> Request<axum::body::Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("Authorization", "Bearer test-secret")
        .body(axum::body::Body::empty())
        .unwrap()
}

fn fixture(
    catalog: Arc<ForbiddenPosterCatalog>,
) -> (axum::Router, MediaId, SubscribeId) {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();

    let state = ApiState::new(
        store,
        "test-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NullFetcher),
        Arc::new(NullDownloader),
        tmp.path().join("library"),
    )
    .unwrap()
    .with_catalog(catalog);

    let store_arc = state.store();
    let store = store_arc.lock();
    let user_id = store.list_users().unwrap().into_iter().find(|u| u.role == UserRole::Admin).unwrap().id;

    let media_id = MediaId::new();
    let media = Media {
        id: media_id,
        kind: MediaKind::Movie,
        title: "Test Movie".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("12345".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();

    let subscribe_id = SubscribeId::new();
    let filter = domain::Filter::new(domain::FilterId::new(), "test-filter", vec![]);
    store.insert_filter(&filter).unwrap();

    let subscribe = Subscribe {
        id: subscribe_id,
        user_id,
        media_id,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: filter.id,
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };
    store.insert_subscribe(&subscribe).unwrap();
    drop(store);

    std::mem::forget(tmp);

    let app = api::router(state);
    (app, media_id, subscribe_id)
}

#[tokio::test]
async fn subscriptions_list_does_not_call_remote_catalog_and_returns_stable_poster_route() {
    let catalog = Arc::new(ForbiddenPosterCatalog::new());
    let (app, media_id, _) = fixture(catalog.clone());

    let req = authed_get("/api/v1/subscriptions");
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let rows = body["data"].as_array().expect("data array");
    assert_eq!(rows.len(), 1);

    assert_eq!(
        catalog.poster_calls.load(Ordering::SeqCst),
        0,
        "subscriptions list must NEVER invoke catalog.poster_url synchronously!"
    );

    let poster_url = rows[0]["media"]["poster_url"].as_str().expect("poster_url should be present");
    assert_eq!(
        poster_url,
        format!("/media/{media_id}/poster"),
        "Cold catalog poster must resolve to stable local /media/{{id}}/poster route"
    );
}

#[tokio::test]
async fn slow_poster_fetch_does_not_hold_store_lock_or_block_jobs_and_auth() {
    let catalog = Arc::new(ForbiddenPosterCatalog::new());
    let (app, _media_id, _) = fixture(catalog.clone());

    // 并发请求 /api/v1/subscriptions, /api/v1/jobs, /api/v1/auth/me
    let concurrent = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let (r1, r2, r3) = tokio::join!(
            app.clone().oneshot(authed_get("/api/v1/subscriptions")),
            app.clone().oneshot(authed_get("/api/v1/jobs?limit=50")),
            app.clone().oneshot(authed_get("/api/v1/auth/me")),
        );
        (r1.unwrap().status(), r2.unwrap().status(), r3.unwrap().status())
    })
    .await;

    assert!(concurrent.is_ok(), "Concurrent requests must not deadlock or exceed timeout");
    let (s1, s2, s3) = concurrent.unwrap();
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(s3, StatusCode::OK);
}
