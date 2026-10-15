use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use domain::MediaId;
use parking_lot::Mutex;
use tokio::sync::Semaphore;

use crate::catalog::Catalog;
use crate::Store;
use super::cache::{DiskPosterCache, NEGATIVE_TTL};
use super::http::{ImageFetcher, RestricedImageFetcher};
use super::inflight::{wait_poster, InflightMap, PosterInflight, PosterWorkerGuard};
use super::negative_cache::{NegativePosterCache, PosterClock, SystemPosterClock};
use super::{MediaPosterSource, PosterBytes, PosterError, PosterResourceCounts};

const NEGATIVE_CACHE_CAPACITY: usize = 4_096;
const GATE_REGISTRY_CAPACITY: usize = 4_096;

pub struct MediaPosterService {
    store: Arc<Mutex<Store>>,
    catalog: Arc<dyn Catalog>,
    cache: DiskPosterCache,
    fetcher: Arc<dyn ImageFetcher>,
    in_flight: Arc<Mutex<InflightMap>>,
    negative_cache: Arc<Mutex<NegativePosterCache>>,
    global_concurrency: Arc<Semaphore>,
    active_workers: Arc<AtomicUsize>,
    wait_deadline: Duration,
}

impl MediaPosterService {
    pub fn new(store: Arc<Mutex<Store>>, catalog: Arc<dyn Catalog>) -> Self {
        let clock = Arc::new(SystemPosterClock::new());
        Self {
            store,
            catalog,
            cache: DiskPosterCache::new(DiskPosterCache::default_dir()),
            fetcher: Arc::new(RestricedImageFetcher),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            negative_cache: Arc::new(Mutex::new(NegativePosterCache::new(
                NEGATIVE_CACHE_CAPACITY,
                NEGATIVE_TTL,
                clock,
            ))),
            global_concurrency: Arc::new(Semaphore::new(4)),
            active_workers: Arc::new(AtomicUsize::new(0)),
            wait_deadline: Duration::from_secs(10),
        }
    }

    pub fn with_custom_fetcher(
        store: Arc<Mutex<Store>>,
        catalog: Arc<dyn Catalog>,
        cache_dir: std::path::PathBuf,
        fetcher: Arc<dyn ImageFetcher>,
    ) -> Self {
        let clock = Arc::new(SystemPosterClock::new());
        Self {
            store,
            catalog,
            cache: DiskPosterCache::new(cache_dir),
            fetcher,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            negative_cache: Arc::new(Mutex::new(NegativePosterCache::new(
                NEGATIVE_CACHE_CAPACITY,
                NEGATIVE_TTL,
                clock,
            ))),
            global_concurrency: Arc::new(Semaphore::new(4)),
            active_workers: Arc::new(AtomicUsize::new(0)),
            wait_deadline: Duration::from_secs(10),
        }
    }

    pub fn with_wait_deadline(mut self, deadline: Duration) -> Self {
        self.wait_deadline = deadline;
        self
    }
    pub fn with_negative_cache_clock(mut self, clock: Arc<dyn PosterClock>) -> Self {
        self.negative_cache = Arc::new(Mutex::new(NegativePosterCache::new(
            NEGATIVE_CACHE_CAPACITY,
            NEGATIVE_TTL,
            clock,
        )));
        self
    }

    pub fn resource_counts(&self) -> PosterResourceCounts {
        let in_flight = self.in_flight.lock();
        let registry_entries = in_flight.len();
        drop(in_flight);
        let negative_entries = self.negative_cache.lock().len();
        let active_workers = self.active_workers.load(Ordering::SeqCst);

        PosterResourceCounts {
            negative_entries,
            registry_entries,
            active_workers,
        }
    }

    /// Diagnostic probe returning the number of in-flight entries.
    /// Not called by production code.
    #[doc(hidden)]
    pub fn inflight_entries(&self) -> usize {
        self.in_flight.lock().len()
    }

    /// Diagnostic probe returning the number of active waiters for a media item.
    /// Returns 0 when no entry exists. Not called by production code.
    #[doc(hidden)]
    pub fn inflight_waiters(&self, media_id: MediaId) -> usize {
        self.in_flight
            .lock()
            .get(&media_id)
            .and_then(|weak| weak.upgrade())
            .map_or(0, |entry| entry.waiter_count())
    }

    /// Test helper to insert a weak inflight gate without exposing the internal map.
    #[doc(hidden)]
    pub fn insert_inflight_for_test(&self, media_id: MediaId, entry: &Arc<PosterInflight>) {
        self.in_flight.lock().insert(media_id, Arc::downgrade(entry));
    }

    /// Test helper to check if a key is present in the negative cache.
    #[doc(hidden)]
    pub fn check_negative_cache_for_test(&self, media_id: MediaId) -> bool {
        self.negative_cache.lock().get(media_id).is_some()
    }

    fn check_negative_cache(&self, media_id: MediaId) -> Option<PosterError> {
        self.negative_cache.lock().get(media_id)
    }

    fn record_negative_cache(
        negative_cache: &Mutex<NegativePosterCache>,
        media_id: MediaId,
        err: &PosterError,
    ) {
        let should_cache = matches!(
            err,
            PosterError::NotFound | PosterError::Upstream(_) | PosterError::Timeout
        );
        if should_cache {
            negative_cache.lock().insert(media_id, err.clone());
        }
    }

    fn spawn_worker(&self, media_id: MediaId, entry: Arc<PosterInflight>) {
        let store = self.store.clone();
        let catalog = self.catalog.clone();
        let fetcher = self.fetcher.clone();
        let cache = self.cache.clone();
        let sem = self.global_concurrency.clone();
        let in_flight = self.in_flight.clone();
        let negative_cache = self.negative_cache.clone();
        let active_workers = self.active_workers.clone();
        let mut guard = PosterWorkerGuard::new(media_id, entry.clone(), in_flight);
        tokio::spawn(async move {
            active_workers.fetch_add(1, Ordering::SeqCst);
            struct ActiveWorkerGuard(Arc<AtomicUsize>);
            impl Drop for ActiveWorkerGuard {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let _worker_guard = ActiveWorkerGuard(active_workers);

            let permit = match sem.acquire_owned().await {
                Ok(p) => p,
                Err(_) => {
                    guard.complete(Err(PosterError::Io("concurrency semaphore closed".into())));
                    return;
                }
            };
            guard.set_permit(permit);

            let _ = tokio::task::spawn_blocking(move || {
                let outcome = (|| -> Result<PosterBytes, PosterError> {
                    if let Some(hit) = cache.get(media_id) {
                        return Ok(hit);
                    }

                    let (kind, tmdb_id) = {
                        let s = store.lock();
                        let media = s.get_media(media_id)
                            .map_err(|e| PosterError::Io(e.to_string()))?
                            .ok_or(PosterError::NotFound)?;
                        let tmdb_id = media.tmdb_id.clone().ok_or(PosterError::NotFound)?;
                        (media.kind, tmdb_id)
                    };

                    let url = catalog
                        .poster_url(kind, &tmdb_id)
                        .map_err(PosterError::Upstream)?
                        .ok_or(PosterError::NotFound)?;

                    let (content_type, bytes) = fetcher.fetch(&url)?;
                    cache.put(media_id, &content_type, &bytes)?;

                    Ok(PosterBytes {
                        content_type,
                        bytes: Arc::from(bytes.into_boxed_slice()),
                    })
                })();

                if let Err(err) = &outcome {
                    Self::record_negative_cache(&negative_cache, media_id, err);
                }

                guard.complete(outcome);
            })
            .await;
        });
    }
}

#[async_trait::async_trait]
impl MediaPosterSource for MediaPosterService {
    async fn get(&self, media_id: MediaId) -> Result<PosterBytes, PosterError> {
        if let Some(err) = self.check_negative_cache(media_id) {
            return Err(err);
        }

        let (entry, is_leader) = {
            let mut map = self.in_flight.lock();
            // Clean dead entries before checking capacity / admitting new entry
            map.retain(|_, weak| weak.upgrade().is_some());

            if let Some(weak) = map.get(&media_id) {
                if let Some(existing) = weak.upgrade() {
                    (existing, false)
                } else {
                    if map.len() >= GATE_REGISTRY_CAPACITY {
                        return Err(PosterError::Busy);
                    }
                    let new_entry = Arc::new(PosterInflight::new());
                    map.insert(media_id, Arc::downgrade(&new_entry));
                    (new_entry, true)
                }
            } else {
                if map.len() >= GATE_REGISTRY_CAPACITY {
                    return Err(PosterError::Busy);
                }
                let new_entry = Arc::new(PosterInflight::new());
                map.insert(media_id, Arc::downgrade(&new_entry));
                (new_entry, true)
            }
        };

        if is_leader {
            self.spawn_worker(media_id, entry.clone());
        }
        let res = wait_poster(entry, self.wait_deadline).await;

        match res {
            Ok(bytes) => {
                tracing::info!(media_id = %media_id, "海报获取成功");
                Ok(bytes)
            }
            Err(err) => {
                let outcome = match &err {
                    PosterError::NotFound => "not_found",
                    PosterError::Timeout => "timeout",
                    PosterError::InvalidImage => "invalid_image",
                    PosterError::Upstream(_) => "upstream",
                    PosterError::Io(_) => "io",
                    PosterError::Busy => "busy",
                };
                if matches!(err, PosterError::Timeout) {
                    tracing::warn!(media_id = %media_id, outcome, "海报等待超时（后台任务继续运行）");
                } else {
                    tracing::error!(media_id = %media_id, outcome, "海报获取失败");
                }
                Err(err)
            }
        }
    }
}
