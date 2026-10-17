//! A3：Jellyfin 先选择后重元数据、NextUp 一次聚合的 API 级证据。
//!
//! 覆盖 SCOPE-6-01..04 的回归，以及分页/分集展开/folder/unknown parent/排序 ties/
//! StartIndex/最新默认播放状态等既有组合的等价性。
//!
//! 工作量用注入的 [`BrowseFileRead`] 计数：`external_subtitles` 每次重元数据构建
//! 调用一次（tracks/chapters/NFO/stat 都在同一处），`metadata` 是 stat 次数。断言
//! 只用公开协议 JSON + 这份 seam，不依赖实现内部类型。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use api::browse_file_read::BrowseFileRead;
use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::Value;
use tower::ServiceExt;

const TOKEN: &str = "management-secret";

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

type Hook = Box<dyn Fn() + Send + Sync>;

/// 计数浏览文件 seam：重元数据构建（NFO/stat/字幕发现）必须只发生在选中的页上。
#[derive(Default)]
struct CountingFiles {
    metadata: AtomicUsize,
    subtitles: AtomicUsize,
    nfo: AtomicUsize,
    on_first_io: parking_lot::Mutex<Option<Hook>>,
}

impl CountingFiles {
    fn subtitles(&self) -> usize {
        self.subtitles.load(Ordering::SeqCst)
    }

    fn metadata(&self) -> usize {
        self.metadata.load(Ordering::SeqCst)
    }

    fn reset(&self) {
        self.metadata.store(0, Ordering::SeqCst);
        self.subtitles.store(0, Ordering::SeqCst);
        self.nfo.store(0, Ordering::SeqCst);
    }

    /// 第一次文件 IO 时执行一次（模拟请求进行中的存储故障）。
    fn fail_store_on_first_io(&self, hook: Hook) {
        *self.on_first_io.lock() = Some(hook);
    }

    fn fire_once(&self) {
        let hook = self.on_first_io.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }
}

impl BrowseFileRead for CountingFiles {
    fn read_nfo(&self, path: &Path) -> Option<library::NfoMeta> {
        self.nfo.fetch_add(1, Ordering::SeqCst);
        library::read_nfo(path)
    }

    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        self.fire_once();
        self.metadata.fetch_add(1, Ordering::SeqCst);
        std::fs::metadata(path)
    }

    fn external_subtitles(&self, path: &Path) -> Result<Vec<library::SubtitleTrack>, String> {
        self.fire_once();
        self.subtitles.fetch_add(1, Ordering::SeqCst);
        library::external_subtitle_tracks(path).map_err(|error| error.to_string())
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    state: ApiState,
    files: Arc<CountingFiles>,
    library_id: String,
    roots: Vec<PathBuf>,
}

fn new_state(dir: &Path, files: Arc<CountingFiles>) -> ApiState {
    let store = Store::open(dir.join("data")).unwrap();
    ApiState::new(
        store,
        TOKEN.into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.join("stage"))),
        dir.join("library"),
    )
    .unwrap()
    .with_browse_file_read(files)
}

fn new_media(store: &Store, title: &str) -> MediaId {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: title.into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    media.id
}

fn insert_episode(store: &Store, dir: &Path, media: MediaId, season: u32, episode: u32) -> LedgerId {
    let path = dir.join(format!("{season:02}x{episode:02}.mkv"));
    std::fs::write(&path, b"episode").unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: media,
        path: path.display().to_string(),
        season: Some(season),
        episode: Some(episode),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).unwrap();
    row.id
}

/// TV 库：`shows` 部剧、每部 `episodes` 集，全部挂在同一个根下。
fn tv_fixture(shows: usize, episodes: usize) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let files = Arc::new(CountingFiles::default());
    let state = new_state(dir.path(), files.clone());
    let root = dir.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    let library_id = {
        let store = state.store();
        let store = store.lock();
        store
            .create_library(
                MediaKind::Tv,
                "Shows",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
    };
    {
        let store = state.store();
        let store = store.lock();
        for show in 0..shows {
            let media_id = new_media(&store, &format!("Show{show:03}"));
            let show_dir = root.join(format!("Show{show:03}"));
            std::fs::create_dir_all(&show_dir).unwrap();
            for episode in 1..=episodes {
                insert_episode(&store, &show_dir, media_id, 1, episode as u32);
            }
        }
    }
    Fixture {
        dir,
        state,
        files,
        library_id,
        roots: vec![root],
    }
}

/// 按标题找 Media（测试 fixture 里标题唯一）。
fn media_id_for_title(state: &ApiState, title: &str) -> String {
    let store = state.store();
    let store = store.lock();
    for row in store.list_ledger().unwrap() {
        let Some(media) = store.get_media(row.media_id).unwrap() else {
            continue;
        };
        if media.title == title {
            return media.id.to_string().replace('-', "");
        }
    }
    panic!("media {title} not found");
}

fn folder_id(root: &Path) -> String {
    format!(
        "folder_{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, root.to_string_lossy().as_bytes()).simple()
    )
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn get_json(app: &axum::Router, uri: &str) -> Value {
    let (status, body) = get(app, uri).await;
    assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    body
}

fn items(body: &Value) -> &Vec<Value> {
    body["Items"].as_array().expect("Items array")
}

#[path = "jellyfin_paged_metadata/paged.rs"]
mod paged;
#[path = "jellyfin_paged_metadata/scope.rs"]
mod scope;
#[path = "jellyfin_paged_metadata/next_up.rs"]
mod next_up;
#[path = "jellyfin_paged_metadata/audit.rs"]
mod audit;

