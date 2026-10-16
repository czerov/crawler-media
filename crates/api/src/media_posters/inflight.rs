use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use domain::MediaId;
use parking_lot::Mutex;
use tokio::sync::{Notify, OwnedSemaphorePermit};

use super::{PosterBytes, PosterError};

pub type InflightMap = HashMap<MediaId, Weak<PosterInflight>>;

pub struct PosterInflight {
    pub outcome: Mutex<Option<Result<PosterBytes, PosterError>>>,
    pub changed: Notify,
    waiters: AtomicUsize,
}

impl PosterInflight {
    pub fn new() -> Self {
        Self {
            outcome: Mutex::new(None),
            changed: Notify::new(),
            waiters: AtomicUsize::new(0),
        }
    }

    pub fn waiter_count(&self) -> usize {
        self.waiters.load(Ordering::SeqCst)
    }

    pub fn publish(&self, result: Result<PosterBytes, PosterError>) {
        {
            let mut out = self.outcome.lock();
            if out.is_none() {
                *out = Some(result);
            }
        }
        self.changed.notify_waiters();
    }
}

impl Default for PosterInflight {
    fn default() -> Self {
        Self::new()
    }
}

pub struct PosterWorkerGuard {
    media_id: MediaId,
    entry: Arc<PosterInflight>,
    permit: Option<OwnedSemaphorePermit>,
    in_flight: Arc<Mutex<InflightMap>>,
    started: std::time::Instant,
    completed: bool,
}

impl PosterWorkerGuard {
    pub fn new(
        media_id: MediaId,
        entry: Arc<PosterInflight>,
        in_flight: Arc<Mutex<InflightMap>>,
    ) -> Self {
        Self {
            media_id,
            entry,
            permit: None,
            in_flight,
            started: std::time::Instant::now(),
            completed: false,
        }
    }

    pub fn set_permit(&mut self, permit: OwnedSemaphorePermit) {
        self.permit = Some(permit);
    }

    pub fn complete(&mut self, result: Result<PosterBytes, PosterError>) {
        if !self.completed {
            let outcome = match &result {
                Ok(_) => "ok",
                Err(PosterError::NotFound) => "not_found",
                Err(PosterError::Timeout) => "timeout",
                Err(PosterError::InvalidImage) => "invalid_image",
                Err(PosterError::Upstream(_)) => "upstream",
                Err(PosterError::Io(_)) => "io",
                Err(PosterError::Busy) => "busy",
            };
            let elapsed_ms = self.started.elapsed().as_millis() as u64;
            tracing::info!(
                media_id = %self.media_id,
                outcome,
                elapsed_ms,
                "海报 worker 执行完成"
            );
            self.entry.publish(result);
            self.completed = true;
        }
    }
}

impl Drop for PosterWorkerGuard {
    fn drop(&mut self) {
        if !self.completed {
            let elapsed_ms = self.started.elapsed().as_millis() as u64;
            tracing::error!(
                media_id = %self.media_id,
                outcome = "panic",
                elapsed_ms,
                "海报 worker 异常终止未发布终态，发布 Io 错误并移除 inflight 条目"
            );
            self.entry.publish(Err(PosterError::Io("poster worker terminated unexpectedly".into())));
            self.completed = true;
        }

        // Identity-checked retirement: older worker can never remove newer generation's entry
        // Clean up from map only if the weak entry points to our entry or is dead
        let mut map = self.in_flight.lock();
        if let Some(weak) = map.get(&self.media_id) {
            if weak.upgrade().is_none_or(|current| Arc::ptr_eq(&current, &self.entry)) {
                map.remove(&self.media_id);
            }
        }
    }
}

struct WaiterGuard<'a>(&'a AtomicUsize);

impl<'a> Drop for WaiterGuard<'a> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub async fn wait_poster(
    entry: Arc<PosterInflight>,
    deadline: Duration,
) -> Result<PosterBytes, PosterError> {
    // 1. Fast path: check before registering notification or incrementing waiter count
    if let Some(res) = entry.outcome.lock().clone() {
        return res;
    }

    entry.waiters.fetch_add(1, Ordering::SeqCst);
    let _guard = WaiterGuard(&entry.waiters);

    // 2. Loop with notified(): register first, then re-check terminal state
    // so no wakeup is lost between notify_waiters and await
    let wait_fut = async {
        loop {
            let notified = entry.changed.notified();
            if let Some(res) = entry.outcome.lock().clone() {
                return res;
            }
            notified.await;
            if let Some(res) = entry.outcome.lock().clone() {
                return res;
            }
        }
    };

    match tokio::time::timeout(deadline, wait_fut).await {
        Ok(res) => res,
        Err(_) => {
            tracing::warn!("海报等待请求超时");
            Err(PosterError::Timeout)
        }
    }
}
