use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

fn app(root: &Path) -> (axum::Router, MediaId, String) {
    let store = Store::open(root.join("data")).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: None,
        original_title: None,
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let file = root.join("matrix.mkv");
    std::fs::write(&file, b"0123456789abcdef").unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: file.display().to_string(),
        season: None,
        episode: None,
        resolution: Some("2160p".into()),
        codec: Some("hevc".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(100),
    };
    store.insert_ledger(&row).unwrap();
    let compact = row.id.to_string().replace('-', "");
    let router = router(
        ApiState::new(
            store,
            "admin-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(root.join("stage"))),
            root.join("library"),
        )
        .unwrap(),
    );
    (router, media.id, compact)
}

struct FakeClock(std::sync::atomic::AtomicI64);

impl api::playback_events::PlaybackClock for FakeClock {
    fn now_ms(&self) -> i64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

fn app_with_clock(root: &Path, clock: Arc<dyn api::playback_events::PlaybackClock>) -> (axum::Router, MediaId, String) {
    let store = Store::open(root.join("data")).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: None,
        original_title: None,
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let file = root.join("matrix.mkv");
    std::fs::write(&file, b"0123456789abcdef").unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: file.display().to_string(),
        season: None,
        episode: None,
        resolution: Some("2160p".into()),
        codec: Some("hevc".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(100),
    };
    store.insert_ledger(&row).unwrap();
    let compact = row.id.to_string().replace('-', "");
    let state = ApiState::new(
        store,
        "admin-token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
    .with_playback_clock(clock);
    (router(state), media.id, compact)
}

async fn json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[path = "jellyfin/authentication_routes.rs"]
mod authentication_routes;
#[path = "jellyfin/endpoint_routes.rs"]
mod endpoint_routes;
#[path = "jellyfin/library_routes.rs"]
mod library_routes;
#[path = "jellyfin/user_marks_routes.rs"]
mod user_marks_routes;

#[path = "jellyfin/catalog.rs"]
mod catalog;
#[path = "jellyfin/playback.rs"]
mod playback;
#[path = "jellyfin/protocol.rs"]
mod protocol;
#[path = "jellyfin/visibility.rs"]
mod visibility;

#[path = "jellyfin/filters.rs"]
mod filters;
#[path = "jellyfin/metadata_parity.rs"]
mod metadata_parity;
#[path = "jellyfin/metadata_scrape.rs"]
mod metadata_scrape;
#[path = "jellyfin/people.rs"]
mod people;
