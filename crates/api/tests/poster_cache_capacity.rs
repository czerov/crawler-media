use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::http::{Request, StatusCode};
use domain::{Media, MediaId, MediaKind, Torrent};
use downloader::{Downloader, DownloaderError};
use indexer::{Fetcher, FetchRequest, IndexerError, ProfileSet};
use media::CatalogHit;
use parking_lot::{Condvar, Mutex};
use tower::ServiceExt;

use api::catalog::Catalog;
use api::management::ApiState;
use api::media_posters::cache::{DiskPosterCache, MAX_IMAGE_BYTES};
use api::media_posters::http::ImageFetcher;
use api::media_posters::service::MediaPosterService;
use api::media_posters::{MediaPosterSource, PosterClock, PosterError};
use api::Store;

struct MockCatalog;

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
        if tmdb_id.starts_with("not_found") {
            Ok(None)
        } else {
            Ok(Some(format!("https://image.tmdb.org/t/p/w500/{tmdb_id}.jpg")))
        }
    }
}

struct ManualClock {
    now_ms: AtomicUsize,
}

impl ManualClock {
    fn new() -> Self {
        Self {
            now_ms: AtomicUsize::new(0),
        }
    }

    fn advance(&self, duration: Duration) {
        self.now_ms.fetch_add(duration.as_millis() as usize, Ordering::SeqCst);
    }

    fn rewind(&self, duration: Duration) {
        let rewind_ms = duration.as_millis() as usize;
        let _ = self.now_ms.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |curr| {
            Some(curr.saturating_sub(rewind_ms))
        });
    }
}

impl PosterClock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.now_ms.load(Ordering::SeqCst) as u64)
    }
}

struct SimpleFetcher;

impl ImageFetcher for SimpleFetcher {
    fn fetch(&self, _url: &str) -> Result<(String, Vec<u8>), PosterError> {
        Ok(("image/jpeg".into(), vec![0xFF, 0xD8, 0xFF, 0xE0]))
    }
}

struct TestGate {
    released: Mutex<bool>,
    cvar: Condvar,
}

impl TestGate {
    fn new() -> Self {
        Self {
            released: Mutex::new(false),
            cvar: Condvar::new(),
        }
    }

    fn wait(&self) {
        let mut rel = self.released.lock();
        while !*rel {
            self.cvar.wait(&mut rel);
        }
    }

    fn release(&self) {
        let mut rel = self.released.lock();
        *rel = true;
        self.cvar.notify_all();
    }
}

struct GatedFetcher {
    gate: Arc<TestGate>,
    pub active: AtomicUsize,
    pub fetch_calls: AtomicUsize,
}

impl GatedFetcher {
    fn new(gate: Arc<TestGate>) -> Self {
        Self {
            gate,
            active: AtomicUsize::new(0),
            fetch_calls: AtomicUsize::new(0),
        }
    }
}

impl ImageFetcher for GatedFetcher {
    fn fetch(&self, _url: &str) -> Result<(String, Vec<u8>), PosterError> {
        self.fetch_calls.fetch_add(1, Ordering::SeqCst);
        self.active.fetch_add(1, Ordering::SeqCst);
        self.gate.wait();
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(("image/jpeg".into(), vec![0xFF, 0xD8, 0xFF, 0xE0]))
    }
}

fn create_store_and_service(
    fetcher: Arc<dyn ImageFetcher>,
) -> (tempfile::TempDir, Arc<Mutex<Store>>, MediaPosterService) {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let cache_dir = tmp.path().join("cache");
    let store = Arc::new(Mutex::new(Store::open(&data_dir).unwrap()));
    let catalog = Arc::new(MockCatalog);
    let service = MediaPosterService::with_custom_fetcher(
        store.clone(),
        catalog,
        cache_dir,
        fetcher,
    );
    (tmp, store, service)
}

fn insert_media(store: &Store, title: &str, tmdb_id: Option<&str>) -> MediaId {
    let id = MediaId::new();
    let media = Media {
        id,
        kind: MediaKind::Movie,
        title: title.into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: tmdb_id.map(str::to_string),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    id
}

#[tokio::test]
async fn distinct_negative_keys_are_bounded_and_expire_without_revisit() {
    let (_tmp, store, service) = create_store_and_service(Arc::new(SimpleFetcher));
    let clock = Arc::new(ManualClock::new());
    let service = service.with_negative_cache_clock(clock.clone());

    // Set clock to positive baseline so backward movement can be tested
    clock.advance(Duration::from_secs(100));

    // 1. Insert 4,096 distinct media with tmdb_id that triggers NotFound
    let mut keys = Vec::with_capacity(4096);
    for i in 0..4096 {
        let media_id = insert_media(&store.lock(), &format!("not_found_{i}"), Some("not_found"));
        let res = service.get(media_id).await;
        assert!(matches!(res, Err(PosterError::NotFound)));
        keys.push(media_id);
    }

    let counts = service.resource_counts();
    assert_eq!(counts.negative_entries, 4_096);

    // Touch keys[0] so it is promoted to MRU; FIFO would leave keys[0] at the front
    let res0 = service.get(keys[0]).await;
    assert!(matches!(res0, Err(PosterError::NotFound)));

    // Insert a new key (4,097th). LRU must evict keys[1] (least recently used) while keys[0] survives
    let new_key = insert_media(&store.lock(), "not_found_new", Some("not_found"));
    let res_new = service.get(new_key).await;
    assert!(matches!(res_new, Err(PosterError::NotFound)));

    // Assert LRU: touched keys[0] survived while keys[1] was evicted.
    assert!(service.check_negative_cache_for_test(keys[0]), "keys[0] was touched so it must survive eviction");
    assert!(!service.check_negative_cache_for_test(keys[1]), "keys[1] was least recently used so it must be evicted");

    // Move clock backward past the insertion time (e.g. from 100s to 50s)
    clock.rewind(Duration::from_secs(50));

    // Assert backward-clock handling: keys[0] must still be cached (not prematurely expired)
    assert!(service.check_negative_cache_for_test(keys[0]), "entry inserted at positive clock must still be cached after backward clock move");
    let counts_rewound = service.resource_counts();
    assert_eq!(counts_rewound.negative_entries, 4_096);
    // Advance clock past expiration (30s TTL, move to 100s + 31s = 131s)
    clock.advance(Duration::from_secs(81));

    // A READ must sweep on its own, without relying on a later insert: query an absent
    // key (a miss that does not insert) and assert the expired entries are already gone.
    let absent_id = insert_media(&store.lock(), "not_found_absent", Some("not_found"));
    assert!(!service.check_negative_cache_for_test(absent_id));
    assert_eq!(
        service.resource_counts().negative_entries,
        0,
        "a negative-cache read must sweep expired entries by itself"
    );

    // Next access to any key sweeps expired entries
    let dummy_id = insert_media(&store.lock(), "not_found_dummy", Some("not_found"));
    let _ = service.get(dummy_id).await;

    let counts_after = service.resource_counts();
    // After sweeping expired entries, only the newly inserted dummy entry should remain
    assert_eq!(counts_after.negative_entries, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_waiters_and_disk_double_check_leave_no_dead_gate() {
    let gate = Arc::new(TestGate::new());
    let fetcher = Arc::new(GatedFetcher::new(gate.clone()));
    let (_tmp, store, service) = create_store_and_service(fetcher.clone());
    let service = Arc::new(service.with_wait_deadline(Duration::from_millis(100)));

    let media_id = insert_media(&store.lock(), "cancelled_waiter_test", Some("12345"));

    // Spawn waiter that will timeout / get cancelled
    let s = service.clone();
    let handle = tokio::spawn(async move { s.get(media_id).await });

    // Wait until worker is inside fetcher gate
    while fetcher.active.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Waiter times out
    let res = handle.await.unwrap();
    assert!(matches!(res, Err(PosterError::Timeout)));

    // Release gate so worker completes and puts poster into disk cache
    gate.release();

    // Give worker a moment to finish disk write & complete
    while fetcher.active.load(Ordering::SeqCst) > 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Wait for the original worker to finish and retire its inflight entry before the
    // second request, so the second request must open a fresh gate and be served from
    // the DISK cache rather than joining the running worker or refetching upstream.
    let retire_deadline = std::time::Instant::now() + Duration::from_secs(2);
    while service.inflight_entries() > 0 {
        assert!(
            std::time::Instant::now() < retire_deadline,
            "the worker must retire its inflight entry after publishing"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let fetches_before = fetcher.fetch_calls.load(Ordering::SeqCst);

    // Subsequent access hits disk double-check path directly
    let cached_res = service.get(media_id).await;
    assert!(cached_res.is_ok());
    assert_eq!(
        fetcher.fetch_calls.load(Ordering::SeqCst),
        fetches_before,
        "a disk-cache hit must not perform another upstream fetch"
    );

    // Verify registry is cleaned of dead gates. Retirement happens in the worker's
    // guard drop, which runs after the outcome is published, so wait for it bounded.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let counts = service.resource_counts();
        if counts.registry_entries == 0 && counts.active_workers == 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "registry must not retain dead gates: registry_entries={} active_workers={}",
            counts.registry_entries,
            counts.active_workers
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn full_live_registry_returns_busy_and_same_key_still_joins() {
    let (_tmp, store, service) = create_store_and_service(Arc::new(SimpleFetcher));
    let service = Arc::new(service);

    // Populate registry with live gates up to 4096
    // We hold strong references to keep gates alive
    let mut live_gates = Vec::new();
    for i in 0..4096 {
        let media_id = insert_media(&store.lock(), &format!("live_gate_{i}"), Some("100"));
        let inflight = Arc::new(api::media_posters::inflight::PosterInflight::new());
        service.insert_inflight_for_test(media_id, &inflight);
        live_gates.push((media_id, inflight));
    }

    let counts = service.resource_counts();
    assert_eq!(counts.registry_entries, 4096);

    // Requesting a NEW key when registry is full of live gates yields Busy (mapped to 503)
    let new_media_id = insert_media(&store.lock(), "new_gate_overflow", Some("200"));
    let err = service.get(new_media_id).await;
    assert!(matches!(err, Err(PosterError::Busy)));

    // Requesting an EXISTING live key still joins the existing gate (does not return Busy)
    let (existing_id, existing_inflight) = &live_gates[0];
    existing_inflight.publish(Ok(api::media_posters::PosterBytes {
        content_type: "image/jpeg".into(),
        bytes: Arc::from([0x01, 0x02].as_slice()),
    }));
    let joined = service.get(*existing_id).await;
    assert!(joined.is_ok());

    // Drop strong references to all 4,096 live gates: they are now dead entries.
    drop(live_gates);

    // Note: resource_counts is strictly observational (no retain/prune), so it still
    // observes 4,096 dead weak entries before any new request.
    assert_eq!(service.resource_counts().registry_entries, 4096);

    // Prove admission behaviour directly without any intervening diagnostic call:
    // requesting a NEW key must sweep dead entries during admission and succeed.
    let admitted_id = insert_media(&store.lock(), "admitted_after_drop", Some("100"));
    let admitted_res = service.get(admitted_id).await;
    assert!(admitted_res.is_ok(), "new key must be admitted after strong references dropped");

    // The admission above is the real proof: with 4,096 dead entries unswept the new
    // key would have been rejected as Busy instead of served. Every entry (including
    // the new one) retires in the worker guard drop after publication, so wait for the
    // registry to drain rather than expecting the fresh entry to still be registered.
    let drain_deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let counts_swept = service.resource_counts();
        if counts_swept.registry_entries == 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < drain_deadline,
            "dead entries must be swept and live entries retired: registry_entries={}",
            counts_swept.registry_entries
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[test]
fn max_image_bytes_round_trips_through_disk_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = DiskPosterCache::new(tmp.path());
    let media_id = MediaId::new();

    // Exactly MAX_IMAGE_BYTES (8 MiB)
    let large_image = vec![0x42u8; MAX_IMAGE_BYTES];
    let put_res = cache.put(media_id, "image/jpeg", &large_image);
    assert!(put_res.is_ok(), "putting exactly 8 MiB image must succeed: {:?}", put_res.err());

    let get_res = cache.get(media_id);
    assert!(get_res.is_some(), "getting exactly 8 MiB image must succeed");
    let poster = get_res.unwrap();
    assert_eq!(poster.content_type, "image/jpeg");
    assert_eq!(poster.bytes.len(), MAX_IMAGE_BYTES);
    assert_eq!(&poster.bytes[..], &large_image[..]);

    // Boundary case: exactly MAX_META_BYTES metadata allowance
    // Format is: "{now_secs()}\n{content_type}\n\n{img}"
    // meta_prefix is "{now_secs()}\n{content_type}\n"
    // If content_type is sized so meta_prefix is exactly MAX_META_BYTES:
    let now_prefix = format!("{}\n", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
    let ct_len = api::media_posters::cache::MAX_META_BYTES - now_prefix.len() - 1; // -1 for trailing newline in meta_prefix
    let max_ct = "a".repeat(ct_len);
    let boundary_id = MediaId::new();
    let put_bound = cache.put(boundary_id, &max_ct, &large_image);
    assert!(put_bound.is_ok(), "boundary meta allowance put must succeed: {:?}", put_bound.err());
    let get_bound = cache.get(boundary_id);
    assert!(get_bound.is_some(), "boundary meta allowance get must succeed");
    assert_eq!(get_bound.unwrap().content_type, max_ct);

    // Oversized metadata must be rejected
    let oversized_ct = "a".repeat(ct_len + 1);
    let oversized_id = MediaId::new();
    let put_oversized = cache.put(oversized_id, &oversized_ct, b"small");
    assert!(matches!(put_oversized, Err(PosterError::InvalidImage)));
}
struct NullFetcher;
impl Fetcher for NullFetcher {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Ok(String::new())
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

#[tokio::test]
async fn public_handler_surfaces_busy_as_service_unavailable_503() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let cache_dir = tmp.path().join("cache");
    let store = Arc::new(Mutex::new(Store::open(&data_dir).unwrap()));
    let catalog = Arc::new(MockCatalog);

    let service = Arc::new(MediaPosterService::with_custom_fetcher(
        store.clone(),
        catalog,
        cache_dir,
        Arc::new(SimpleFetcher),
    ));

    // Fill registry to 4096 live gates
    let mut live_gates = Vec::new();
    for i in 0..4096 {
        let media_id = insert_media(&store.lock(), &format!("h_live_{i}"), Some("100"));
        let inflight = Arc::new(api::media_posters::inflight::PosterInflight::new());
        service.insert_inflight_for_test(media_id, &inflight);
        live_gates.push(inflight);
    }

    let overflow_id = insert_media(&store.lock(), "h_overflow", Some("100"));

    let state = ApiState::new_arc(
        store,
        "test-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NullFetcher),
        Arc::new(NullDownloader),
        tmp.path().join("library"),
    )
    .unwrap();

    let state = state.with_media_posters(service);
    let app = api::router(state);

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/media/{overflow_id}/poster"))
        .body(axum::body::Body::empty())
        .unwrap();

    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    assert_eq!(&body[..], b"poster service busy");

    drop(live_gates);
}
