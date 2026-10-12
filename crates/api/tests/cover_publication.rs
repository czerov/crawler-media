//! C2b：Library 封面版本化 CAS 发布。
//!
//! 渲染结果先落成新的不可变版本文件，DB 指针最后 CAS 发布；晚到的自动渲染
//! 不得覆盖更新的手动封面或 `none` 标记。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use api::library_cover_service::{CoverRenderer, LibraryCoverService};
use api::{ApiState, Store, router};
use cover_generator::CoverOptions;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

fn make_state(root: &Path) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
}

struct Gate {
    entered: std::sync::Mutex<bool>,
    entered_cv: std::sync::Condvar,
    open: std::sync::Mutex<bool>,
    open_cv: std::sync::Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            entered: std::sync::Mutex::new(false),
            entered_cv: std::sync::Condvar::new(),
            open: std::sync::Mutex::new(false),
            open_cv: std::sync::Condvar::new(),
        }
    }

    fn enter_and_wait(&self) {
        *self.entered.lock().unwrap() = true;
        self.entered_cv.notify_all();
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.open_cv.wait(open).unwrap();
        }
    }

    fn entered(&self) -> bool {
        *self.entered.lock().unwrap()
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.open_cv.notify_all();
    }
}

/// 卡住渲染，直到测试放行。
struct GatedRenderer {
    gate: Arc<Gate>,
    bytes: Vec<u8>,
}

impl CoverRenderer for GatedRenderer {
    fn render(
        &self,
        _library: &store::Library,
        _rows: &[domain::LedgerRow],
        _options: &CoverOptions,
    ) -> Result<Option<Vec<u8>>, String> {
        self.gate.enter_and_wait();
        Ok(Some(self.bytes.clone()))
    }
}

/// 立即返回固定字节的渲染器（不阻塞）。
struct FixedRenderer(Vec<u8>);

impl CoverRenderer for FixedRenderer {
    fn render(
        &self,
        _library: &store::Library,
        _rows: &[domain::LedgerRow],
        _options: &CoverOptions,
    ) -> Result<Option<Vec<u8>>, String> {
        Ok(Some(self.0.clone()))
    }
}

async fn wait_entered(gate: &Arc<Gate>) {
    let entered = tokio::time::timeout(Duration::from_secs(5), async {
        while !gate.entered() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(entered.is_ok(), "cover renderer was never invoked");
}

fn seed_library(state: &ApiState, root: &Path) -> String {
    let library_root = root.join("covers-library");
    std::fs::create_dir_all(&library_root).unwrap();
    let file = library_root.join("Movie.mkv");
    std::fs::write(&file, b"video").unwrap();
    std::fs::write(library_root.join("poster.jpg"), b"poster").unwrap();
    let store = state.store();
    let guard = store.lock();
    let library = guard
        .create_library(
            MediaKind::Movie,
            "Cover Library",
            &[library_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Cover Movie".into(),
        year: None,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    guard.insert_media(&media).unwrap();
    guard
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: media.id,
            path: file.display().to_string(),
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
    library.id
}

fn cover_path(state: &ApiState, library_id: &str) -> Option<String> {
    let store = state.store();
    let guard = store.lock();
    guard.get_library(library_id).unwrap().unwrap().cover_path
}

fn referenced_bytes(state: &ApiState, library_id: &str) -> Option<Vec<u8>> {
    let path = cover_path(state, library_id)?;
    std::fs::read(path).ok()
}

fn cover_dir_files(state: &ApiState) -> Vec<PathBuf> {
    let dir = {
        let store = state.store();
        let guard = store.lock();
        guard.library_covers_dir()
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    files.sort();
    files
}

fn gated_state(root: &Path, rendered: &[u8]) -> (ApiState, Arc<Gate>) {
    let gate = Arc::new(Gate::new());
    let service = LibraryCoverService::new(Arc::new(GatedRenderer {
        gate: gate.clone(),
        bytes: rendered.to_vec(),
    }));
    let state = make_state(root).with_cover_service(Arc::new(service));
    (state, gate)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_auto_render_does_not_overwrite_manual_cover() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, gate) = gated_state(tmp.path(), b"auto-rendered-cover");
    let library_id = seed_library(&state, tmp.path());

    let auto_state = state.clone();
    let auto_library = library_id.clone();
    let auto = tokio::spawn(async move {
        auto_state
            .cover_service()
            .ensure_cover(&auto_state, &auto_library)
            .await
    });
    wait_entered(&gate).await;

    // 渲染被卡住期间，用户手动上传了更新的封面。
    let manual = b"manual-cover-bytes".to_vec();
    let manual_path = state
        .cover_service()
        .replace_cover(&state, &library_id, manual.clone())
        .await
        .unwrap();
    gate.release();

    let auto_result = auto.await.unwrap();
    assert!(
        auto_result.is_err(),
        "late auto render must fail its revision CAS, got {auto_result:?}"
    );
    assert_eq!(
        PathBuf::from(cover_path(&state, &library_id).unwrap()),
        manual_path
    );
    assert_eq!(referenced_bytes(&state, &library_id).unwrap(), manual);
    assert!(
        !cover_dir_files(&state).iter().any(|path| path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().contains("auto"))),
        "unreferenced auto version must be discarded"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleted_cover_is_not_republished_by_late_render() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, gate) = gated_state(tmp.path(), b"auto-rendered-cover");
    let library_id = seed_library(&state, tmp.path());

    let auto_state = state.clone();
    let auto_library = library_id.clone();
    let auto = tokio::spawn(async move {
        auto_state
            .cover_service()
            .ensure_cover(&auto_state, &auto_library)
            .await
    });
    wait_entered(&gate).await;

    state
        .cover_service()
        .remove_cover(&state, &library_id)
        .await
        .unwrap();
    gate.release();

    assert!(auto.await.unwrap().is_err());
    assert_eq!(cover_path(&state, &library_id).as_deref(), Some("none"));
    assert!(referenced_bytes(&state, &library_id).is_none());

    // 显式删除后，新的 ensure_cover 也不得回填。
    assert!(
        state
            .cover_service()
            .ensure_cover(&state, &library_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frozen_library_does_not_publish_late_render() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, gate) = gated_state(tmp.path(), b"auto-rendered-cover");
    let library_id = seed_library(&state, tmp.path());

    let auto_state = state.clone();
    let auto_library = library_id.clone();
    let auto = tokio::spawn(async move {
        auto_state
            .cover_service()
            .ensure_cover(&auto_state, &auto_library)
            .await
    });
    wait_entered(&gate).await;

    api::library_lifecycle::start_library_removal(&state, &library_id).unwrap();
    gate.release();

    assert!(
        auto.await.unwrap().is_err(),
        "frozen library must not publish a new cover revision"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn db_failure_keeps_previous_cover_and_drops_new_version() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let library_id = seed_library(&state, tmp.path());

    let first = b"first-manual-cover".to_vec();
    let first_path = state
        .cover_service()
        .replace_cover(&state, &library_id, first.clone())
        .await
        .unwrap();
    let before = cover_dir_files(&state);

    // DB 写失败：cover_path 更新被 trigger 拒绝。
    let db_path = {
        let store = state.store();
        let guard = store.lock();
        guard.sqlite_path().to_path_buf()
    };
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_cover_update BEFORE UPDATE OF cover_path ON libraries
             BEGIN SELECT RAISE(ABORT, 'cover db down'); END;",
        )
        .unwrap();

    let error = state
        .cover_service()
        .replace_cover(&state, &library_id, b"second-cover".to_vec())
        .await
        .expect_err("DB failure must surface as an error");
    assert!(!error.is_empty());
    assert_eq!(
        PathBuf::from(cover_path(&state, &library_id).unwrap()),
        first_path
    );
    assert_eq!(referenced_bytes(&state, &library_id).unwrap(), first);
    assert_eq!(
        cover_dir_files(&state),
        before,
        "unreferenced new version must be deleted after a DB failure"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preview_only_does_not_write_files_or_db() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path()).with_cover_service(Arc::new(LibraryCoverService::new(
        Arc::new(FixedRenderer(b"preview-cover-bytes".to_vec())),
    )));
    let library_id = seed_library(&state, tmp.path());
    let before = cover_dir_files(&state);
    let revision_before = {
        let store = state.store();
        let guard = store.lock();
        guard.library_cover_revision(&library_id).unwrap()
    };

    let generated = state
        .cover_service()
        .generate(&state, &library_id, cover_options("Preview"), true)
        .await
        .unwrap();
    assert!(!generated.bytes.is_empty());
    assert!(generated.published_path.is_none());
    assert_eq!(cover_dir_files(&state), before);
    assert_eq!(cover_path(&state, &library_id), None);
    let revision_after = {
        let store = state.store();
        let guard = store.lock();
        guard.library_cover_revision(&library_id).unwrap()
    };
    assert_eq!(revision_before, revision_after);
}

fn cover_options(title: &str) -> CoverOptions {
    CoverOptions {
        title_zh: title.into(),
        title_en: None,
        width: 1920,
        height: 1080,
        style: cover_generator::CoverStyle::MultiPosterPile,
        background: cover_generator::BackgroundOption::default(),
    }
}

// router 只是为了复用 api 的公开导出，避免测试只依赖内部结构。
#[allow(dead_code)]
fn _router(state: &ApiState) -> axum::Router {
    router(state.clone())
}
