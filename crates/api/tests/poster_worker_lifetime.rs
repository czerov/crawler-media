use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use domain::{Media, MediaId, MediaKind};
use media::CatalogHit;
use parking_lot::{Condvar, Mutex};

use api::catalog::Catalog;
use api::media_posters::http::ImageFetcher;
use api::media_posters::service::MediaPosterService;
use api::media_posters::{MediaPosterSource, PosterError};
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
        if tmdb_id == "panic" {
            Ok(Some("https://image.tmdb.org/t/p/w500/panic.jpg".into()))
        } else if tmdb_id == "err" {
            Ok(Some("https://image.tmdb.org/t/p/w500/err.jpg".into()))
        } else {
            Ok(Some(format!("https://image.tmdb.org/t/p/w500/{tmdb_id}.jpg")))
        }
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

struct GateReleaseGuard(Arc<TestGate>);

impl Drop for GateReleaseGuard {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct GatedFetcher {
    active: AtomicUsize,
    max_active: AtomicUsize,
    fetch_calls: AtomicUsize,
    gate: Arc<TestGate>,
}

impl GatedFetcher {
    fn new(gate: Arc<TestGate>) -> Self {
        Self {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            fetch_calls: AtomicUsize::new(0),
            gate,
        }
    }
}

impl ImageFetcher for GatedFetcher {
    fn fetch(&self, url: &str) -> Result<(String, Vec<u8>), PosterError> {
        self.fetch_calls.fetch_add(1, Ordering::SeqCst);
        let curr = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(curr, Ordering::SeqCst);

        self.gate.wait();

        self.active.fetch_sub(1, Ordering::SeqCst);

        if url.contains("panic.jpg") {
            panic!("worker simulated panic");
        } else if url.contains("err.jpg") {
            Err(PosterError::InvalidImage)
        } else {
            Ok(("image/png".into(), vec![0x89, 0x50, 0x4E, 0x47]))
        }
    }
}

fn setup_service_and_store(
    fetcher: Arc<dyn ImageFetcher>,
    catalog: Arc<dyn Catalog>,
    deadline: Duration,
) -> (tempfile::TempDir, Arc<Mutex<Store>>, Arc<MediaPosterService>) {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let cache_dir = tmp.path().join("cache");
    let store = Arc::new(Mutex::new(Store::open(&data_dir).unwrap()));

    let service = Arc::new(
        MediaPosterService::with_custom_fetcher(
            store.clone(),
            catalog,
            cache_dir,
            fetcher,
        )
        .with_wait_deadline(deadline),
    );

    (tmp, store, service)
}

fn insert_media(store: &Store, tmdb_id: &str) -> MediaId {
    let id = MediaId::new();
    let media = Media {
        id,
        kind: MediaKind::Movie,
        title: format!("Movie {tmdb_id}"),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some(tmdb_id.into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    id
}

async fn wait_until<F: Fn() -> bool>(desc: &str, condition: F) {
    let start = std::time::Instant::now();
    let cap = Duration::from_secs(2);
    let interval = Duration::from_millis(5);
    while !condition() {
        if start.elapsed() >= cap {
            panic!("condition wait timed out after {cap:?}: {desc}");
        }
        tokio::time::sleep(interval).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_requests_does_not_release_upstream_slots() {
    let gate = Arc::new(TestGate::new());
    let _guard = GateReleaseGuard(gate.clone());
    let fetcher = Arc::new(GatedFetcher::new(gate.clone()));
    let catalog = Arc::new(MockCatalog {
        poster_calls: AtomicUsize::new(0),
    });

    let (_tmp, store, service) = setup_service_and_store(
        fetcher.clone(),
        catalog.clone(),
        Duration::from_secs(10),
    );

    let m1 = insert_media(&store.lock(), "m1");
    let m2 = insert_media(&store.lock(), "m2");
    let m3 = insert_media(&store.lock(), "m3");
    let m4 = insert_media(&store.lock(), "m4");

    // Spawn 4 requests and abort them
    let mut handles = Vec::new();
    for id in [m1, m2, m3, m4] {
        let s = service.clone();
        handles.push(tokio::spawn(async move { s.get(id).await }));
    }

    // Wait until 4 are active in fetcher
    wait_until("4 workers active in fetcher", || {
        fetcher.active.load(Ordering::SeqCst) >= 4
    }).await;

    // Abort all 4 client requests
    for h in handles {
        h.abort();
    }
    // Verify that aborting client callers didn't cause workers to drop their active slots
    assert_eq!(
        fetcher.active.load(Ordering::SeqCst),
        4,
        "active workers must stay 4 immediately after client cancellation"
    );

    // Now request 4 MORE media
    let m5 = insert_media(&store.lock(), "m5");
    let m6 = insert_media(&store.lock(), "m6");
    let m7 = insert_media(&store.lock(), "m7");
    let m8 = insert_media(&store.lock(), "m8");

    let mut second_handles = Vec::new();
    for id in [m5, m6, m7, m8] {
        let s = service.clone();
        second_handles.push(tokio::spawn(async move { s.get(id).await }));
    }

    // Give the second batch a bounded window in which the concurrency bound must hold continuously
    let poll_start = std::time::Instant::now();
    while poll_start.elapsed() < Duration::from_millis(300) {
        let active = fetcher.active.load(Ordering::SeqCst);
        let max_active = fetcher.max_active.load(Ordering::SeqCst);
        if active > 4 || max_active > 4 {
            panic!("concurrency bound violated: active={active}, max_active={max_active} (must be <= 4)");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // While gate is closed, active must remain 4 and max_active must NOT exceed 4
    let active_before_release = fetcher.active.load(Ordering::SeqCst);
    let max_active_before_release = fetcher.max_active.load(Ordering::SeqCst);

    // Release gate so workers can finish
    gate.release();
    for h in second_handles {
        let res = h.await.unwrap();
        assert!(res.is_ok(), "second batch request should succeed: {:?}", res.err());
    }

    // Wait for all active to reach 0
    wait_until("all workers finished", || {
        fetcher.active.load(Ordering::SeqCst) == 0
    }).await;

    // Wait for all inflight entries to be retired
    wait_until("all inflight entries retired", || {
        service.inflight_entries() == 0
    }).await;
    assert_eq!(service.inflight_entries(), 0);

    assert_eq!(active_before_release, 4, "4 initial workers should still hold slots");
    assert_eq!(
        max_active_before_release, 4,
        "max_active must NOT exceed 4 even when cancelled callers abort"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_media_after_timeout_joins_running_fetch() {
    let gate = Arc::new(TestGate::new());
    let _guard = GateReleaseGuard(gate.clone());
    let fetcher = Arc::new(GatedFetcher::new(gate.clone()));
    let catalog = Arc::new(MockCatalog {
        poster_calls: AtomicUsize::new(0),
    });

    let (_tmp, store, service) = setup_service_and_store(
        fetcher.clone(),
        catalog.clone(),
        Duration::from_millis(50), // short injected wait deadline
    );

    let m1 = insert_media(&store.lock(), "join_test");

    // Request 1: will time out at 50ms wait deadline
    let s1 = service.clone();
    let res1 = s1.get(m1).await;
    assert!(matches!(res1, Err(PosterError::Timeout)));

    // Ensure worker 1 is actively inside fetcher
    wait_until("worker 1 active in fetcher", || {
        fetcher.active.load(Ordering::SeqCst) == 1
    }).await;

    // While worker is still gated and running, entry must be retained
    assert_eq!(fetcher.fetch_calls.load(Ordering::SeqCst), 1);
    assert_eq!(service.inflight_entries(), 1);

    // Spawn a second request while worker is running; ensure no second fetch starts
    let s2 = service.clone();
    let handle2 = tokio::spawn(async move { s2.get(m1).await });

    wait_until("follower waiting or finished", || {
        service.inflight_waiters(m1) >= 1 || handle2.is_finished()
    }).await;
    assert_eq!(
        fetcher.fetch_calls.load(Ordering::SeqCst),
        1,
        "no second fetch starts for media while first is inflight"
    );
    assert_eq!(service.inflight_entries(), 1);

    gate.release();
    let _ = handle2.await;

    wait_until("worker completed and retired inflight", || {
        fetcher.active.load(Ordering::SeqCst) == 0 && service.inflight_entries() == 0
    }).await;
    assert_eq!(
        fetcher.fetch_calls.load(Ordering::SeqCst),
        1,
        "fetch calls must remain 1 after completion"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn follower_after_cancelled_leader_receives_outcome() {
    let gate = Arc::new(TestGate::new());
    let _guard = GateReleaseGuard(gate.clone());
    let fetcher = Arc::new(GatedFetcher::new(gate.clone()));
    let catalog = Arc::new(MockCatalog {
        poster_calls: AtomicUsize::new(0),
    });

    let (_tmp, store, service) = setup_service_and_store(
        fetcher.clone(),
        catalog.clone(),
        Duration::from_secs(10),
    );

    let m1 = insert_media(&store.lock(), "follower_outcome_test");

    let s1 = service.clone();
    let h_leader = tokio::spawn(async move { s1.get(m1).await });

    wait_until("leader active in fetcher", || {
        fetcher.active.load(Ordering::SeqCst) == 1
    }).await;

    // Abort leader task and await it; worker must keep running and entry must stay
    h_leader.abort();
    let _ = h_leader.await;
    assert_eq!(fetcher.active.load(Ordering::SeqCst), 1);
    assert_eq!(service.inflight_entries(), 1);

    let s2 = service.clone();
    let h_follower = tokio::spawn(async move { s2.get(m1).await });

    wait_until("follower waiting", || {
        service.inflight_waiters(m1) >= 1
    }).await;

    gate.release();

    let follower_res = h_follower.await.unwrap();
    assert!(follower_res.is_ok(), "follower must succeed: {:?}", follower_res.err());
    assert_eq!(
        fetcher.fetch_calls.load(Ordering::SeqCst),
        1,
        "fetch calls must stay 1"
    );

    let follower_bytes = follower_res.unwrap();
    assert_eq!(&*follower_bytes.bytes, &[0x89, 0x50, 0x4E, 0x47]);

    // Compare follower result with a subsequent request. The follower consumed the
    // worker's single published outcome (it succeeded after the leader was aborted
    // with fetch_calls == 1, so the entry survived cancellation); the later request
    // is served by a fresh disk-cache read, which allocates its own bytes, so the two
    // Arcs must differ.
    let cached_res = service.get(m1).await.unwrap();
    assert_eq!(follower_bytes.bytes, cached_res.bytes);
    assert!(
        !Arc::ptr_eq(&follower_bytes.bytes, &cached_res.bytes),
        "a disk-cache read must not share the published outcome's allocation"
    );
}


#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leader_and_follower_share_one_published_outcome() {
    let catalog = Arc::new(MockCatalog {
        poster_calls: AtomicUsize::new(0),
    });
    // Positive replay proof with a long wait deadline so neither caller times out.
    // Both leader and follower observe the single published in-memory outcome.
    let gate_long = Arc::new(TestGate::new());
    let _guard_long = GateReleaseGuard(gate_long.clone());
    let fetcher_long = Arc::new(GatedFetcher::new(gate_long.clone()));
    let (_tmp_long, store_long, service_long) = setup_service_and_store(
        fetcher_long.clone(),
        catalog.clone(),
        Duration::from_secs(10),
    );
    let m_long = insert_media(&store_long.lock(), "replay_proof");

    let s_leader = service_long.clone();
    let h_leader = tokio::spawn(async move { s_leader.get(m_long).await });
    wait_until("leader active in fetcher", || {
        fetcher_long.active.load(Ordering::SeqCst) == 1
    }).await;

    let s_follower = service_long.clone();
    let h_follower = tokio::spawn(async move { s_follower.get(m_long).await });
    wait_until("leader and follower waiting and fetcher gated", || {
        service_long.inflight_waiters(m_long) >= 2 && fetcher_long.active.load(Ordering::SeqCst) == 1
    }).await;

    gate_long.release();
    let leader_res = h_leader.await.unwrap();
    let follower_res = h_follower.await.unwrap();
    assert!(leader_res.is_ok(), "leader must succeed: {:?}", leader_res.err());
    assert!(follower_res.is_ok(), "follower must succeed: {:?}", follower_res.err());
    assert_eq!(fetcher_long.fetch_calls.load(Ordering::SeqCst), 1, "fetch count must stay 1");
    let leader_bytes = leader_res.unwrap();
    let follower_bytes = follower_res.unwrap();
    assert_eq!(leader_bytes.bytes, follower_bytes.bytes);
    assert!(
        Arc::ptr_eq(&leader_bytes.bytes, &follower_bytes.bytes),
        "leader and follower bytes must share the exact same Arc pointer from single published inflight outcome"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cache_write_failure_fails_leader_and_follower() {
    let catalog = Arc::new(MockCatalog {
        poster_calls: AtomicUsize::new(0),
    });
    // Production contract characterization: unwritable cache directory.
    // A failed cache write currently fails the request (bytes are not served);
    // this test pins existing production behavior rather than endorsing it.
    let gate_fail = Arc::new(TestGate::new());
    let _guard_fail = GateReleaseGuard(gate_fail.clone());
    let fetcher_fail = Arc::new(GatedFetcher::new(gate_fail.clone()));
    let tmp_fail = tempfile::tempdir().unwrap();
    let data_dir_fail = tmp_fail.path().join("data");
    let cache_file_blocker = tmp_fail.path().join("cache_blocker");
    std::fs::write(&cache_file_blocker, b"blocking_file").unwrap();
    let store_fail = Arc::new(Mutex::new(Store::open(&data_dir_fail).unwrap()));
    let service_fail = Arc::new(
        MediaPosterService::with_custom_fetcher(
            store_fail.clone(),
            catalog.clone(),
            cache_file_blocker,
            fetcher_fail.clone(),
        )
        .with_wait_deadline(Duration::from_secs(10)),
    );
    let m_fail = insert_media(&store_fail.lock(), "fail_cache_media");

    let s_fail_leader = service_fail.clone();
    let h_fail_leader = tokio::spawn(async move { s_fail_leader.get(m_fail).await });
    wait_until("fail leader active in fetcher", || {
        fetcher_fail.active.load(Ordering::SeqCst) == 1
    }).await;

    let s_fail_follower = service_fail.clone();
    let h_fail_follower = tokio::spawn(async move { s_fail_follower.get(m_fail).await });
    wait_until("fail leader and follower waiting and fetcher still gated", || {
        service_fail.inflight_waiters(m_fail) >= 2 && fetcher_fail.active.load(Ordering::SeqCst) == 1
    }).await;

    gate_fail.release();
    let res_leader = h_fail_leader.await.unwrap();
    let res_follower = h_fail_follower.await.unwrap();
    assert!(matches!(res_leader, Err(PosterError::Io(_))), "leader must see Io error on unwritable cache: {:?}", res_leader);
    assert!(matches!(res_follower, Err(PosterError::Io(_))), "follower must see Io error on unwritable cache: {:?}", res_follower);
    assert_eq!(fetcher_fail.fetch_calls.load(Ordering::SeqCst), 1, "fetch count must stay 1");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn error_outcome_replays_and_releases_permits() {
    let gate = Arc::new(TestGate::new());
    let _guard = GateReleaseGuard(gate.clone());
    let fetcher = Arc::new(GatedFetcher::new(gate.clone()));
    let catalog = Arc::new(MockCatalog {
        poster_calls: AtomicUsize::new(0),
    });

    let (_tmp, store, service) = setup_service_and_store(
        fetcher.clone(),
        catalog.clone(),
        Duration::from_secs(5),
    );

    let m_err = insert_media(&store.lock(), "err");

    // Leader request
    let s1 = service.clone();
    let h1 = tokio::spawn(async move { s1.get(m_err).await });

    // Wait until in fetcher
    wait_until("leader in fetcher", || {
        fetcher.active.load(Ordering::SeqCst) >= 1
    }).await;

    let s2 = service.clone();
    let h2 = tokio::spawn(async move { s2.get(m_err).await });

    wait_until("leader and follower waiting", || {
        service.inflight_waiters(m_err) >= 2
    }).await;
    gate.release();
    let r1 = h1.await.unwrap();
    let r2 = h2.await.unwrap();

    // InvalidImage is not recorded in the negative cache (worker caches only NotFound, Upstream, Timeout).
    // Both leader and follower receiving PosterError::InvalidImage with fetch_calls == 1 proves the
    // follower was served by replaying the worker's in-memory published outcome, not by negative cache.
    assert!(matches!(r1, Err(PosterError::InvalidImage)));
    assert!(matches!(r2, Err(PosterError::InvalidImage)));
    assert_eq!(fetcher.fetch_calls.load(Ordering::SeqCst), 1);

    // R2-03: prove permit and entry release on the SAME service
    *fetcher.gate.released.lock() = false;
    let cap_m1 = insert_media(&store.lock(), "err_cap1");
    let cap_m2 = insert_media(&store.lock(), "err_cap2");
    let cap_m3 = insert_media(&store.lock(), "err_cap3");
    let cap_m4 = insert_media(&store.lock(), "err_cap4");

    let mut cap_handles = Vec::new();
    for id in [cap_m1, cap_m2, cap_m3, cap_m4] {
        let s = service.clone();
        cap_handles.push(tokio::spawn(async move { s.get(id).await }));
    }

    wait_until("4 distinct media active in fetcher", || {
        fetcher.active.load(Ordering::SeqCst) == 4
    }).await;
    assert_eq!(fetcher.active.load(Ordering::SeqCst), 4);
    assert_eq!(fetcher.max_active.load(Ordering::SeqCst), 4);

    fetcher.gate.release();
    for h in cap_handles {
        let _ = h.await;
    }

    wait_until("all capacity workers drained", || {
        fetcher.active.load(Ordering::SeqCst) == 0
    }).await;

    wait_until("error media inflight entry retired", || {
        service.inflight_entries() == 0
    }).await;

    let calls_before = fetcher.fetch_calls.load(Ordering::SeqCst);
    let r3 = service.get(m_err).await;
    assert!(matches!(r3, Err(PosterError::InvalidImage)));
    assert_eq!(
        fetcher.fetch_calls.load(Ordering::SeqCst),
        calls_before + 1,
        "new generation for error media must increment fetch count"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_panic_releases_resources_and_replays_failure() {
    let gate = Arc::new(TestGate::new());
    let _guard = GateReleaseGuard(gate.clone());
    let fetcher = Arc::new(GatedFetcher::new(gate.clone()));
    let catalog = Arc::new(MockCatalog {
        poster_calls: AtomicUsize::new(0),
    });

    let (_tmp, store, service) = setup_service_and_store(
        fetcher.clone(),
        catalog.clone(),
        Duration::from_secs(5),
    );

    let m_panic = insert_media(&store.lock(), "panic");

    let s1 = service.clone();
    let h1 = tokio::spawn(async move { s1.get(m_panic).await });

    wait_until("leader in fetcher", || {
        fetcher.active.load(Ordering::SeqCst) >= 1
    }).await;

    let s2 = service.clone();
    let h2 = tokio::spawn(async move { s2.get(m_panic).await });

    wait_until("leader and follower waiting", || {
        service.inflight_waiters(m_panic) >= 2
    }).await;

    // Release gate
    gate.release();
    let r1 = h1.await.unwrap();
    let r2 = h2.await.unwrap();

    assert!(matches!(r1, Err(PosterError::Io(_))));
    assert!(matches!(r2, Err(PosterError::Io(_))));
    assert_eq!(fetcher.fetch_calls.load(Ordering::SeqCst), 1, "fetch_calls must be 1 (no second fetch)");

    // Wait until worker completely finishes and permit is released
    wait_until("panic worker finished and slot released", || {
        fetcher.active.load(Ordering::SeqCst) == 0
    }).await;

    // Assert capacity is intact on the SAME service by submitting four distinct new media
    // and observing active == 4 with max_active == 4 while gated
    let cap_m1 = insert_media(&store.lock(), "cap1");
    let cap_m2 = insert_media(&store.lock(), "cap2");
    let cap_m3 = insert_media(&store.lock(), "cap3");
    let cap_m4 = insert_media(&store.lock(), "cap4");

    // Reset gate for the second capacity batch on the same service/fetcher
    let gate2 = Arc::new(TestGate::new());
    let _guard2 = GateReleaseGuard(gate2.clone());
    *fetcher.gate.released.lock() = false;

    let mut cap_handles = Vec::new();
    for id in [cap_m1, cap_m2, cap_m3, cap_m4] {
        let s = service.clone();
        cap_handles.push(tokio::spawn(async move { s.get(id).await }));
    }

    wait_until("4 distinct media active in fetcher", || {
        fetcher.active.load(Ordering::SeqCst) == 4
    }).await;

    assert_eq!(fetcher.active.load(Ordering::SeqCst), 4);
    assert_eq!(fetcher.max_active.load(Ordering::SeqCst), 4);

    fetcher.gate.release();
    for h in cap_handles {
        let _ = h.await;
    }

    wait_until("all capacity workers drained", || {
        fetcher.active.load(Ordering::SeqCst) == 0
    }).await;

    // Assert panic entry itself was retired on the SAME service
    wait_until("panic entry retired on original service", || {
        service.inflight_entries() == 0
    }).await;
    assert_eq!(service.inflight_entries(), 0);

    // Requesting the panic media again on that service after the drain triggers a new fetch
    let fetch_calls_before = fetcher.fetch_calls.load(Ordering::SeqCst);
    let s_retry_panic = service.clone();
    let h_panic_retry = tokio::spawn(async move { s_retry_panic.get(m_panic).await });
    wait_until("panic media re-request active in fetcher", || {
        fetcher.fetch_calls.load(Ordering::SeqCst) > fetch_calls_before
    }).await;
    gate2.release();
    let r_panic_retry = h_panic_retry.await.unwrap();
    assert!(matches!(r_panic_retry, Err(PosterError::Io(_))));
    assert_eq!(fetcher.fetch_calls.load(Ordering::SeqCst), fetch_calls_before + 1);
    // Retirement assertion using a fresh service and successful media:
    // after the entry retires, removing the disk cache entry forces a new fetch.
    let gate3 = Arc::new(TestGate::new());
    let _guard3 = GateReleaseGuard(gate3.clone());
    let fetcher3 = Arc::new(GatedFetcher::new(gate3.clone()));
    let (_tmp3, store3, service3) = setup_service_and_store(
        fetcher3.clone(),
        catalog.clone(),
        Duration::from_secs(5),
    );
    let m_retry = insert_media(&store3.lock(), "retry_test");
    gate3.release();

    let first_res = service3.get(m_retry).await;
    assert!(first_res.is_ok());
    assert_eq!(fetcher3.fetch_calls.load(Ordering::SeqCst), 1);

    wait_until("worker retired inflight entry", || {
        service3.inflight_entries() == 0
    }).await;

    // Now remove from disk cache to test retirement of inflight entry
    let cache_file = _tmp3.path().join("cache").join(format!("{m_retry}.img"));
    let _ = std::fs::remove_file(cache_file);

    // Fresh request for the same media must start a NEW fetch (not replaying old entry)
    let second_res = service3.get(m_retry).await;
    assert!(second_res.is_ok());
    assert_eq!(
        fetcher3.fetch_calls.load(Ordering::SeqCst),
        2,
        "fresh request after cache invalidation must trigger a new fetch rather than replaying stale inflight entry"
    );
}
