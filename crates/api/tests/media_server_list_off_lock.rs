//! C2c：`list_visible_items` 的 metadata 文件 IO 必须在 Store 锁之外。
//!
//! 基线在锁内做这些 IO：gate 被进入时 `try_lock()` 会失败。

use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use api::browse_file_read::BrowseFileRead;
use api::{ApiState, Store};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::provider::MediaServerProvider;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

struct Gate {
    entered: Mutex<bool>,
    entered_cv: Condvar,
    open: Mutex<bool>,
    open_cv: Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            entered: Mutex::new(false),
            entered_cv: Condvar::new(),
            open: Mutex::new(false),
            open_cv: Condvar::new(),
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

/// 断言 panic 也要放行 gate，否则阻塞的 blocking 工作永不结束、测试挂死。
struct ReleaseOnDrop(Arc<Gate>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// 注入的浏览文件访问：provider 路径里可注入的 IO 是 `external_subtitles`
/// （NFO 读取走 `item_metadata/nfo.rs` 的直读，不经过 seam），因此 gate 放在这里。
struct BlockingFiles {
    gate: Arc<Gate>,
}

impl BrowseFileRead for BlockingFiles {
    fn read_nfo(&self, path: &Path) -> Option<library::NfoMeta> {
        library::read_nfo(path)
    }

    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }

    fn external_subtitles(&self, path: &Path) -> Result<Vec<library::SubtitleTrack>, String> {
        self.gate.enter_and_wait();
        library::external_subtitle_tracks(path).map_err(|error| error.to_string())
    }
}

fn make_state(root: &Path, gate: Arc<Gate>) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
    .with_browse_file_read(Arc::new(BlockingFiles { gate }))
}

/// 默认 movie 库下写一个真实视频 + NFO + ledger 行；返回 admin 用户。
fn seed(state: &ApiState, root: &Path) -> domain::UserId {
    let store = state.store();
    let guard = store.lock();
    let library = guard
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|library| library.kind == MediaKind::Movie)
        .expect("default movie library");
    let library_root = library.root_paths.first().cloned().unwrap_or_else(|| {
        let fallback = root.join("media");
        std::fs::create_dir_all(&fallback).unwrap();
        fallback
    });
    std::fs::create_dir_all(&library_root).unwrap();
    let dir = library_root.join("Off Lock (2024)");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("off-lock.mkv");
    std::fs::write(&file, b"video-bytes").unwrap();
    std::fs::write(
        dir.join("off-lock.nfo"),
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<movie><title>Off Lock</title><year>2024</year><genre>Action</genre></movie>\n",
    )
    .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Off Lock".into(),
        year: Some(2024),
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
            resolution: Some("1080p".into()),
            codec: Some("h264".into()),
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();
    guard
        .user_id_by_token("management-secret")
        .unwrap()
        .expect("seeded admin")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_visible_items_reads_metadata_io_outside_the_store_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::new());
    let state = make_state(tmp.path(), gate.clone());
    let user_id = seed(&state, tmp.path());

    let provider = api::media_server_provider::ApiServerProvider::new(state.clone());
    let handle = tokio::spawn(async move { provider.list_visible_items(user_id, None).await });

    let entered = tokio::time::timeout(Duration::from_secs(2), async {
        while !gate.entered() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(entered.is_ok(), "injected browse IO was never entered");
    let _release = ReleaseOnDrop(gate.clone());

    {
        let store = state.store();
        assert!(
            store.try_lock().is_some(),
            "metadata file IO must not run while the Store lock is held"
        );
    }

    gate.release();
    let items = handle.await.unwrap().expect("list_visible_items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].media.title, "Off Lock");
    assert_eq!(
        items[0].metadata.genres,
        vec!["Action".to_string()],
        "NFO metadata must still be read"
    );
}
