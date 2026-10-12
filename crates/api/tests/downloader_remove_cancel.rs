//! R4 回归：下载器 remove RPC 与本地 pending finalization 必须同属一个 owned 作业。
//!
//! 请求 future 在 RPC 阻塞期间被 drop 时，本地 pending 行仍必须被删除；否则会出现
//! “外部任务已删、本地仍是 pending/grabbed”的不一致（要等 15 分钟宽限才自愈）。

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use api::{ApiState, Store, router};
use axum::body::Body;
use axum::http::Request;
use domain::{
    AtomRule, Coverage, FetchMode, Filter, FilterAtom, FilterId, MediaId, SiteId, Subscribe,
    SubscribeId, Torrent, UserId,
};
use downloader::{Downloader, DownloaderError};
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::json;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

/// 让 `remove_owned` 停在闭包内，便于在 RPC 阻塞期间 drop 请求 future。
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

    fn wait_entered(&self) {
        let mut entered = self.entered.lock().unwrap();
        while !*entered {
            entered = self.entered_cv.wait(entered).unwrap();
        }
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.open_cv.notify_all();
    }
}

struct GatedRemover {
    gate: Arc<Gate>,
    removed: AtomicUsize,
    call_thread: Mutex<Option<ThreadId>>,
}

impl Downloader for GatedRemover {
    fn add(&self, _torrent: &Torrent) -> Result<(), DownloaderError> {
        Ok(())
    }

    fn completed_files(
        &self,
        _torrent: &Torrent,
    ) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        Ok(Vec::new())
    }

    fn remove_owned(&self, _torrent: &Torrent, _delete_files: bool) -> Result<(), DownloaderError> {
        *self.call_thread.lock().unwrap() = Some(std::thread::current().id());
        self.removed.fetch_add(1, Ordering::SeqCst);
        self.gate.enter_and_wait();
        Ok(())
    }
}

/// 自带 seeding：filter + subscribe + 一条 pending（downloader_id=None → 默认下载器）。
fn seed_pending(root: &Path, enclosure: &str) -> String {
    let store = Store::open(root.join("data")).unwrap();
    let filter = Filter {
        id: FilterId::new(),
        name: "f".into(),
        atoms: vec![FilterAtom {
            priority: 10,
            rule: AtomRule::Resolution("1080p".into()),
            exclude: false,
        }],
        keep_old_versions: false,
    };
    store.insert_filter(&filter).unwrap();
    let subscribe = Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: MediaId::new(),
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: filter.id,
        wash_cut: false,
        wash_cut_filter_id: None,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
        keep_old_versions: false,
    };
    store.insert_subscribe(&subscribe).unwrap();
    let torrent = Torrent {
        id: None,
        site_id: SiteId::new(),
        title: "Movie.2026.1080p".into(),
        enclosure: enclosure.into(),
        size_bytes: Some(100),
        seeders: Some(1),
        free: false,
        hr: false,
        imdb_id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    };
    store
        .merge_pending(
            subscribe.id,
            &[(
                80,
                api::store::PendingDownload {
                    submitted_at: Some(1),
                    torrent,
                    release_override: None,
                    downloader_id: None,
                },
            )],
        )
        .unwrap();
    format!("{}:{enclosure}", subscribe.id)
}

fn make_state(root: &Path, downloader: Arc<dyn Downloader>) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        downloader,
        root.join("library"),
    )
    .unwrap()
}

fn remove_request(task_id: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/downloaders/tasks/remove")
        .header("content-type", "application/json")
        .header("authorization", "Bearer management-secret")
        .body(Body::from(
            json!({ "task_id": task_id, "delete_files": false }).to_string(),
        ))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_remove_request_still_finalizes_pending() {
    let tmp = tempfile::tempdir().unwrap();
    let enclosure = "https://pt.example/download.php?id=r4";
    let task_id = seed_pending(tmp.path(), enclosure);
    let gate = Arc::new(Gate::new());
    let downloader = Arc::new(GatedRemover {
        gate: gate.clone(),
        removed: AtomicUsize::new(0),
        call_thread: Mutex::new(None),
    });
    let state = make_state(tmp.path(), downloader.clone());
    let app = router(state.clone());

    let handle = tokio::spawn({
        let app = app.clone();
        async move { app.oneshot(remove_request(&task_id)).await.unwrap() }
    });
    gate.wait_entered();
    let call_thread = downloader
        .call_thread
        .lock()
        .unwrap()
        .expect("remove_owned must record its call thread");
    assert_ne!(
        call_thread,
        std::thread::current().id(),
        "remove RPC must run on a blocking worker"
    );

    // drop 请求 future：RPC 已在 owned worker 内，本地 finalization 不能被跳过。
    handle.abort();
    assert!(
        handle.await.is_err(),
        "aborted request future must be cancelled"
    );

    gate.release();

    // 可观测条件轮询 + 超时上限，不猜固定 sleep。
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let pending = state.store().lock().list_all_pending().unwrap();
        if pending.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "pending row survived the dropped request: {pending:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        downloader.removed.load(Ordering::SeqCst),
        1,
        "external remove must have happened exactly once"
    );
}
