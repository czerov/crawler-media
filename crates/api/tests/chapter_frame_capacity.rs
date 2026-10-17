//! Bounded chapter frame dispatch: 200 episodes must never run more than two
//! renderers, never hold more than 64 claimed IDs in memory, and never lose a
//! durable obligation. The runner is a gated fake; no ffmpeg process starts.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use api::chapter_frame_service::{
    chapter_frame_path, ChapterFrameRequest, ChapterFrameRunner, ChapterFrameService,
    ChapterFrameStaged, DISPATCH_WINDOW, MAX_CHAPTER_PROCESSES,
};
use api::library_lifecycle::LibraryWorkRegistry;
use api::Store;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use library::WorkControl;

const FILES: usize = 200;
const TARGETS: [(i64, i64); 2] = [(0, 60_000), (60_000, 120_000)];

struct Gate {
    state: Mutex<(usize, bool)>,
    changed: Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            state: Mutex::new((0, false)),
            changed: Condvar::new(),
        }
    }

    fn enter(&self) {
        let mut state = self.state.lock();
        state.0 += 1;
        self.changed.notify_all();
        while !state.1 {
            self.changed.wait(&mut state);
        }
    }

    fn entered(&self) -> usize {
        self.state.lock().0
    }

    fn release(&self) {
        self.state.lock().1 = true;
        self.changed.notify_all();
    }
}

struct FakeRunner {
    gate: Arc<Gate>,
    active: AtomicUsize,
    max_active: AtomicUsize,
}

impl FakeRunner {
    fn new(gate: Arc<Gate>) -> Self {
        Self {
            gate,
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        }
    }

    fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }
}

impl ChapterFrameRunner for FakeRunner {
    fn render(
        &self,
        request: &ChapterFrameRequest,
        _control: &WorkControl,
    ) -> Result<Vec<ChapterFrameStaged>, String> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        self.gate.enter();
        let mut staged = Vec::new();
        for index in 0..request.targets.len() {
            let target = chapter_frame_path(&request.video, index);
            let staged_path = request.video.with_file_name(format!(
                ".chapter-fake-{}-{index}.jpg",
                request.ledger_id
            ));
            std::fs::write(&staged_path, b"frame").unwrap();
            staged.push(ChapterFrameStaged {
                staged: staged_path,
                target,
            });
        }
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(staged)
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    store: Arc<Mutex<Store>>,
    runner: Arc<FakeRunner>,
    service: Arc<ChapterFrameService>,
    rows: Vec<LedgerRow>,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("data")).unwrap();
        let root = temp.path().join("shows");
        std::fs::create_dir_all(&root).unwrap();
        store
            .create_library(
                MediaKind::Tv,
                "Shows",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media_id = MediaId::new();
        store
            .insert_media(&Media {
                id: media_id,
                kind: MediaKind::Tv,
                title: "Batch".into(),
                year: None,
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let mut rows = Vec::with_capacity(FILES);
        for index in 0..FILES {
            let path = root.join(format!("Batch.S01E{:03}.mkv", index + 1));
            std::fs::write(&path, b"fixture").unwrap();
            let row = LedgerRow {
                id: LedgerId::new(),
                media_id,
                path: path.display().to_string(),
                season: Some(1),
                episode: Some(index as u32 + 1),
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
            };
            store.insert_ledger(&row).unwrap();
            rows.push(row);
        }

        let store = Arc::new(Mutex::new(store));
        let registry = Arc::new(LibraryWorkRegistry::default());
        let gate = Arc::new(Gate::new());
        let runner = Arc::new(FakeRunner::new(gate));
        let service = Arc::new(ChapterFrameService::with_runner(
            store.clone(),
            registry.clone(),
            runner.clone(),
        ));
        Self {
            _temp: temp,
            store,
            runner,
            service,
            rows,
        }
    }

    fn enqueue_all(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| {
                self.service
                    .enqueue_ledger(&row.id.to_string(), TARGETS.to_vec())
                    .unwrap()
            })
            .count()
    }

    fn counts(&self) -> store::ChapterFrameTaskCounts {
        self.store.lock().chapter_frame_task_counts().unwrap()
    }

    fn durable_total(&self) -> usize {
        let counts = self.counts();
        counts.queued + counts.running + counts.completed + counts.failed
    }

    fn published_frames(&self, row: &LedgerRow) -> usize {
        let path = PathBuf::from(&row.path);
        (0..TARGETS.len())
            .filter(|index| chapter_frame_path(&path, *index).exists())
            .count()
    }

    async fn wait_entered(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while self.runner.gate.entered() < expected {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("fake renderer did not reach the gate");
    }

    async fn drain(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(
                self.service.pending_memory() <= DISPATCH_WINDOW,
                "in-memory window exceeded: {}",
                self.service.pending_memory()
            );
            assert!(
                self.runner.max_active() <= MAX_CHAPTER_PROCESSES,
                "renderer concurrency exceeded: {}",
                self.runner.max_active()
            );
            let counts = self.counts();
            if counts.queued == 0 && counts.running == 0 {
                return;
            }
            self.service.dispatch_once().await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
            assert!(Instant::now() < deadline, "chapter batch did not drain");
        }
    }
}

#[tokio::test]
async fn chapter_batch_keeps_process_and_queue_limits() {
    let fixture = Fixture::new();
    assert_eq!(fixture.enqueue_all(), FILES);
    fixture.service.start();
    fixture.wait_entered(MAX_CHAPTER_PROCESSES).await;

    assert!(fixture.runner.max_active() <= MAX_CHAPTER_PROCESSES);
    assert!(fixture.service.pending_memory() <= DISPATCH_WINDOW);

    fixture.runner.gate.release();
    fixture.drain().await;

    let counts = fixture.counts();
    assert_eq!(counts.completed, FILES);
    assert_eq!(counts.failed, 0);
    assert_eq!(fixture.durable_total(), FILES);
    assert!(fixture.runner.max_active() <= MAX_CHAPTER_PROCESSES);
    assert!(fixture.service.published_images() >= FILES * TARGETS.len());
    let _ = fixture.service.shutdown().await;
}

#[tokio::test]
async fn chapter_capacity_defers_without_losing_requests() {
    let fixture = Fixture::new();
    assert_eq!(fixture.enqueue_all(), FILES);
    fixture.service.start();
    fixture.wait_entered(MAX_CHAPTER_PROCESSES).await;

    // The window is full: the rest stays `queued` in SQLite.
    let gated = fixture.counts();
    assert_eq!(gated.running, DISPATCH_WINDOW);
    assert_eq!(gated.queued, FILES - DISPATCH_WINDOW);
    assert_eq!(fixture.service.pending_memory(), DISPATCH_WINDOW);
    assert_eq!(fixture.durable_total(), FILES);

    // Extra dispatches while full must not widen the window or lose rows.
    for _ in 0..3 {
        assert_eq!(fixture.service.dispatch_once().await.unwrap(), 0);
        assert!(fixture.service.pending_memory() <= DISPATCH_WINDOW);
        assert_eq!(fixture.durable_total(), FILES);
    }

    fixture.runner.gate.release();
    fixture.drain().await;

    let counts = fixture.counts();
    assert_eq!(counts.queued + counts.running + counts.completed, FILES);
    assert_eq!(counts.completed, FILES);
    assert!(fixture.runner.max_active() <= MAX_CHAPTER_PROCESSES);
    let _ = fixture.service.shutdown().await;
}

#[tokio::test]
async fn superseded_source_publishes_no_late_frames() {
    let fixture = Fixture::new();
    assert_eq!(fixture.enqueue_all(), FILES);
    // Replace the first episode after enqueue: its captured source version is
    // now stale, so the late frames must be discarded, not written.
    std::fs::write(&fixture.rows[0].path, b"replaced-fixture-bytes").unwrap();

    fixture.service.start();
    fixture.wait_entered(MAX_CHAPTER_PROCESSES).await;
    fixture.runner.gate.release();
    fixture.drain().await;

    let old_version_published_images = fixture.published_frames(&fixture.rows[0]);
    assert_eq!(old_version_published_images, 0);
    assert_eq!(fixture.counts().completed, FILES);
    assert_eq!(fixture.durable_total(), FILES);
    assert!(
        !std::fs::read_dir(std::path::Path::new(&fixture.rows[0].path).parent().unwrap())
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".chapter-fake-"))
    );
    let _ = fixture.service.shutdown().await;
}
