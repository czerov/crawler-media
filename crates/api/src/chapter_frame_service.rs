//! Durable chapter scene-frame dispatch.
//!
//! The Store owns the obligation; this service owns only a bounded window of
//! claimed IDs. At most [`MAX_CHAPTER_PROCESSES`] frames render at once and at
//! most [`DISPATCH_WINDOW`] obligations are held in memory, so a season sweep
//! can enqueue hundreds of episodes without spawning hundreds of processes.

pub mod worker;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use crate::Store;
use crate::library_lifecycle::{LibraryWorkLease, LibraryWorkRegistry};
use crate::store::ChapterFrameTask;
use library::WorkControl;

/// Rendering processes allowed at once. ffmpeg is the only expensive resource
/// this service owns, so this is the process bound.
pub const MAX_CHAPTER_PROCESSES: usize = 2;
/// Obligations claimed from the durable queue and held in memory.
pub const DISPATCH_WINDOW: usize = 64;

const POLL: Duration = Duration::from_millis(25);

#[derive(Clone, Debug)]
pub struct ChapterFrameRequest {
    pub ledger_id: domain::LedgerId,
    pub video: PathBuf,
    pub source_version: String,
    pub targets: Vec<(i64, i64)>,
}

/// One rendered frame waiting for the publication gate.
#[derive(Clone, Debug)]
pub struct ChapterFrameStaged {
    pub staged: PathBuf,
    pub target: PathBuf,
}

/// Frame renderer seam. Production uses ffmpeg through [`worker`]; tests inject
/// a gated fake so no real process starts.
pub trait ChapterFrameRunner: Send + Sync {
    fn render(
        &self,
        request: &ChapterFrameRequest,
        control: &WorkControl,
    ) -> Result<Vec<ChapterFrameStaged>, String>;
}

pub fn chapter_frame_path(video: &Path, index: usize) -> PathBuf {
    let stem = video
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "video".into());
    video.with_file_name(format!("{stem}-chapter_{index}.jpg"))
}

#[derive(Default)]
struct Inner {
    queue: VecDeque<ChapterFrameTask>,
    running: usize,
}

pub struct ChapterFrameService {
    store: Arc<Mutex<Store>>,
    registry: Arc<LibraryWorkRegistry>,
    runner: Arc<dyn ChapterFrameRunner>,
    inner: Mutex<Inner>,
    fill_lock: Mutex<()>,
    notify: Notify,
    started: AtomicBool,
    shutting_down: AtomicBool,
    workers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    published_images: AtomicUsize,
}

impl ChapterFrameService {
    pub fn new(store: Arc<Mutex<Store>>, registry: Arc<LibraryWorkRegistry>) -> Self {
        Self::with_runner(
            store,
            registry,
            Arc::new(worker::FfmpegChapterFrameRunner::default()),
        )
    }

    pub fn with_runner(
        store: Arc<Mutex<Store>>,
        registry: Arc<LibraryWorkRegistry>,
        runner: Arc<dyn ChapterFrameRunner>,
    ) -> Self {
        Self {
            store,
            registry,
            runner,
            inner: Mutex::new(Inner::default()),
            fill_lock: Mutex::new(()),
            notify: Notify::new(),
            started: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            workers: Mutex::new(Vec::new()),
            published_images: AtomicUsize::new(0),
        }
    }

    /// Start the fixed worker set. Without a Tokio runtime nothing is spawned:
    /// enqueueing stays durable-only, which is the no-runtime contract.
    pub fn start(self: &Arc<Self>) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        if self.started.swap(true, Ordering::SeqCst) {
            self.notify.notify_waiters();
            return;
        }
        self.shutting_down.store(false, Ordering::SeqCst);
        // A previous process may have died between claim and ack.
        if let Err(error) = self.store.lock().recover_chapter_frame_tasks() {
            tracing::warn!(%error, "章节帧任务启动恢复失败");
        }
        let mut workers = self.workers.lock();
        for _ in 0..MAX_CHAPTER_PROCESSES {
            let service = self.clone();
            workers.push(tokio::spawn(async move { service.worker_loop().await }));
        }
        drop(workers);
        self.notify.notify_waiters();
    }

    /// Persist one obligation for a ledger and wake the workers.
    pub fn enqueue_ledger(
        &self,
        ledger_id: &str,
        targets: Vec<(i64, i64)>,
    ) -> Result<bool, String> {
        if targets.is_empty() {
            return Ok(false);
        }
        let compact = ledger_id.replace('-', "");
        let (path, enabled) = {
            let store = self.store.lock();
            let Some(row) = store.get_ledger(&compact).map_err(|error| error.to_string())? else {
                return Ok(false);
            };
            let path = PathBuf::from(&row.path);
            (
                path.clone(),
                crate::http::library_chapters::chapter_images_enabled(&store, &path),
            )
        };
        if !enabled {
            return Ok(false);
        }
        self.enqueue_path_inner(&compact, &path, targets)
    }

    /// Persist one obligation resolved from a ledger path.
    pub fn enqueue_path(
        &self,
        path: &Path,
        targets: Vec<(i64, i64)>,
    ) -> Result<bool, String> {
        if targets.is_empty() {
            return Ok(false);
        }
        let ledger_id = {
            let store = self.store.lock();
            store
                .ledger_by_path(&path.to_string_lossy())
                .map_err(|error| error.to_string())?
                .map(|row| row.id.to_string())
        };
        match ledger_id {
            Some(ledger_id) => self.enqueue_ledger(&ledger_id, targets),
            None => Ok(false),
        }
    }

    fn enqueue_path_inner(
        &self,
        ledger_id: &str,
        path: &Path,
        targets: Vec<(i64, i64)>,
    ) -> Result<bool, String> {
        let source_version = crate::fingerprint_job::current_source_version(path);
        let task = ChapterFrameTask {
            id: uuid::Uuid::new_v4().to_string(),
            ledger_id: domain::LedgerId::from_str(ledger_id)
                .map_err(|error| format!("invalid ledger id: {error}"))?,
            source_version,
            targets_hash: targets_hash(&targets),
            targets,
        };
        let inserted = self
            .store
            .lock()
            .enqueue_chapter_frame_task(&task)
            .map_err(|error| error.to_string())?;
        if inserted {
            self.notify.notify_waiters();
        }
        Ok(inserted)
    }

    /// Claim queued obligations up to the in-memory window. Returns how many
    /// were newly claimed.
    pub async fn dispatch_once(&self) -> Result<usize, String> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err("chapter frame service is shut down".into());
        }
        let claimed = self.fill_window()?;
        if claimed > 0 {
            self.notify.notify_waiters();
        }
        Ok(claimed)
    }

    /// Obligations currently held in memory (queued + rendering).
    pub fn pending_memory(&self) -> usize {
        let inner = self.inner.lock();
        inner.queue.len() + inner.running
    }

    /// Successfully published frames. Test seam and capacity observability.
    pub fn published_images(&self) -> usize {
        self.published_images.load(Ordering::SeqCst)
    }

    /// Best-effort synchronous teardown for service replacement. Unlike
    /// [`Self::shutdown`], a blocking render may still be finishing; its claim
    /// stays `running` and is recovered on the next start.
    pub fn abort(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
        for handle in self.workers.lock().drain(..) {
            handle.abort();
        }
        self.inner.lock().queue.clear();
        self.started.store(false, Ordering::SeqCst);
    }

    /// Cancel in-flight work, join both workers, and return every unacknowledged
    /// claim to the durable queue.
    pub async fn shutdown(&self) -> Result<(), String> {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
        let handles: Vec<_> = self.workers.lock().drain(..).collect();
        for handle in handles {
            let _ = handle.await;
        }
        {
            let mut inner = self.inner.lock();
            inner.queue.clear();
            inner.running = 0;
        }
        self.started.store(false, Ordering::SeqCst);
        self.store
            .lock()
            .recover_chapter_frame_tasks()
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn fill_window(&self) -> Result<usize, String> {
        let _fill = self.fill_lock.lock();
        let capacity = {
            let inner = self.inner.lock();
            DISPATCH_WINDOW.saturating_sub(inner.queue.len() + inner.running)
        };
        if capacity == 0 {
            return Ok(0);
        }
        let pending = self
            .store
            .lock()
            .pending_chapter_frame_tasks(capacity)
            .map_err(|error| error.to_string())?;
        let mut claimed = 0;
        for task in pending {
            let owned = self
                .store
                .lock()
                .claim_chapter_frame_task(&task.id)
                .map_err(|error| error.to_string())?;
            if owned {
                self.inner.lock().queue.push_back(task);
                claimed += 1;
            }
        }
        Ok(claimed)
    }

    fn take_next(&self) -> Option<ChapterFrameTask> {
        let mut inner = self.inner.lock();
        let task = inner.queue.pop_front()?;
        inner.running += 1;
        Some(task)
    }

    fn finish_memory(&self) {
        let mut inner = self.inner.lock();
        inner.running = inner.running.saturating_sub(1);
    }

    async fn worker_loop(self: Arc<Self>) {
        loop {
            if self.shutting_down.load(Ordering::SeqCst) {
                break;
            }
            // Register before filling so an enqueue between the empty check and
            // the await cannot lose its wakeup.
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Err(error) = self.fill_window() {
                tracing::warn!(%error, "章节帧任务派发失败");
            }
            match self.take_next() {
                Some(task) => self.run_task(task).await,
                None => notified.await,
            }
        }
    }

    async fn run_task(self: &Arc<Self>, task: ChapterFrameTask) {
        let resolved = self.resolve_target(&task);
        let (video, lease) = match resolved {
            Ok(resolved) => resolved,
            Err(error) => {
                self.finish_task(&task, false, Some(&error));
                self.finish_memory();
                return;
            }
        };
        let request = ChapterFrameRequest {
            ledger_id: task.ledger_id,
            video: video.clone(),
            source_version: task.source_version.clone(),
            targets: task.targets.clone(),
        };
        let control = WorkControl::default();
        let library_control = lease.as_ref().map(|lease| lease.control().clone());
        let runner = self.runner.clone();
        let job_control = control.clone();
        let publish_video = video.clone();
        let publish_version = task.source_version.clone();
        let publish_lease = lease.clone();
        let mut worker = tokio::task::spawn_blocking(move || {
            let staged = runner.render(&request, &job_control)?;
            worker::publish_staged(
                &staged,
                &publish_video,
                &publish_version,
                publish_lease.as_deref(),
                &job_control,
            )
        });

        let outcome = loop {
            tokio::select! {
                joined = &mut worker => break joined,
                _ = tokio::time::sleep(POLL) => {
                    let library_cancelled = library_control
                        .as_ref()
                        .is_some_and(|library| library.is_cancelled());
                    if library_cancelled || self.shutting_down.load(Ordering::SeqCst) {
                        control.cancel();
                    }
                }
            }
        };
        if self.shutting_down.load(Ordering::SeqCst) {
            // Leave the row `running`; shutdown() requeues it after the join.
        } else {
            match outcome {
                Ok(Ok(published)) => {
                    self.published_images.fetch_add(published, Ordering::SeqCst);
                    self.finish_task(&task, true, None);
                }
                Ok(Err(error)) => self.finish_task(&task, false, Some(&error)),
                Err(join) => self.finish_task(
                    &task,
                    false,
                    Some(&format!("chapter frame worker failed: {join}")),
                ),
            }
        }
        self.finish_memory();
    }

    fn resolve_target(
        &self,
        task: &ChapterFrameTask,
    ) -> Result<(PathBuf, Option<Arc<LibraryWorkLease>>), String> {
        let compact = task.ledger_id.to_string().replace('-', "");
        let (path, library_id) = {
            let store = self.store.lock();
            let Some(row) = store.get_ledger(&compact).map_err(|error| error.to_string())? else {
                return Err("file_deleted: ledger row missing".into());
            };
            let path = PathBuf::from(&row.path);
            let library_id = store
                .get_media(row.media_id)
                .map_err(|error| error.to_string())?
                .and_then(|media| {
                    store
                        .library_for_path_strict(&path, media.kind)
                        .ok()
                        .flatten()
                })
                .map(|library| library.id);
            (path, library_id)
        };
        let lease = match library_id {
            Some(library_id) => Some(Arc::new(
                self.registry
                    .acquire(&self.store, &library_id)
                    .map_err(|error| format!("library frozen: {error}"))?,
            )),
            None => None,
        };
        Ok((path, lease))
    }

    fn finish_task(&self, task: &ChapterFrameTask, succeeded: bool, error: Option<&str>) {
        if let Err(error) = self
            .store
            .lock()
            .finish_chapter_frame_task(&task.id, succeeded, error)
        {
            tracing::error!(%error, task = %task.id, "章节帧任务终态写入失败");
        }
    }
}

fn targets_hash(targets: &[(i64, i64)]) -> String {
    let mut digest = Sha256::new();
    for (start, end) in targets {
        digest.update(start.to_le_bytes());
        digest.update(end.to_le_bytes());
    }
    format!("{:x}", digest.finalize())
}
