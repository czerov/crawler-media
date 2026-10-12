//! 共享 single-flight 字幕交付回归：同 key 只跑一次、取消语义、warm cache 与
//! 发布前 source_version 复核。FakeRunner 只写 fixture bytes，不启动任何进程。
//!
//! 等待顺序一律用**确定性握手**，不用 sleep 猜测调度：第二个请求由测试自己
//! poll 一次推进到「已加入同一 in-flight 任务」，worker 结束用 fake runner 的
//! finished 计数观察。

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::Poll;
use std::time::{Duration, Instant};

use api::subtitle_delivery::{SubtitleDelivery, SubtitleRequest, SubtitleServiceError};
use api::{ApiState, Store};
use domain::{LedgerId, MediaKind};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use library::{LibraryError, SubtitleRunner, SubtitleTrack, Tracks, WorkControl};

const INDEX: u32 = 2;

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

fn exit_status(code: i32) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw(code << 8)
}

struct FakeRunner {
    gate: Option<Arc<Gate>>,
    body: Vec<u8>,
    /// `output` 被调用次数。
    calls: Arc<AtomicUsize>,
    /// `output` 返回次数：用于确认提取工作真的结束过，再断言没有残留文件。
    finished: Arc<AtomicUsize>,
    /// 模拟外部工具在提取后替换同路径源文件。
    mutate_source: Option<PathBuf>,
}

impl FakeRunner {
    fn new(
        gate: Option<Arc<Gate>>,
        body: &[u8],
        calls: Arc<AtomicUsize>,
        finished: Arc<AtomicUsize>,
        mutate_source: Option<PathBuf>,
    ) -> Self {
        Self {
            gate,
            body: body.to_vec(),
            calls,
            finished,
            mutate_source,
        }
    }
}

impl SubtitleRunner for FakeRunner {
    fn output(&self, command: &mut Command, control: &WorkControl) -> Result<Output, LibraryError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let staged = PathBuf::from(
            command
                .get_args()
                .last()
                .expect("command must end with the staging path"),
        );
        if let Some(gate) = &self.gate {
            gate.enter_and_wait();
        }
        // 先落盘再检查取消：清理逻辑必须删掉本任务的 staging。
        std::fs::write(&staged, &self.body).unwrap();
        if let Some(source) = &self.mutate_source {
            std::fs::write(source, b"externally-replaced-source-bytes").unwrap();
        }
        self.finished.fetch_add(1, Ordering::SeqCst);
        if control.is_cancelled() {
            return Err(LibraryError::Cancelled);
        }
        Ok(Output {
            status: exit_status(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }
}

fn make_state(
    root: &Path,
    runner: Arc<dyn SubtitleRunner>,
    concurrency: usize,
) -> (ApiState, Arc<SubtitleDelivery>) {
    let service = Arc::new(SubtitleDelivery::new(runner, concurrency));
    let state = ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
    .with_subtitle_delivery(service.clone());
    (state, service)
}

fn request(ledger: LedgerId, source: &Path) -> SubtitleRequest {
    SubtitleRequest {
        ledger_id: ledger,
        source_path: source.to_path_buf(),
        kind: MediaKind::Tv,
        tracks: Tracks {
            subtitles: vec![SubtitleTrack {
                stream_index: Some(INDEX),
                codec: Some("subrip".into()),
                is_external: false,
                path: None,
                ..Default::default()
            }],
            ..Default::default()
        },
        index: INDEX,
        wants_vtt: true,
    }
}

/// 与 service 内部一致的 cache 目录（使用 service 暴露的布局 helper）。
fn cache_dir(ledger: LedgerId, source: &Path) -> PathBuf {
    let version = api::fingerprint_job::current_source_version(source);
    api::subtitle_delivery::subtitle_cache_dir(ledger, &version)
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read.filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect()
}

fn fixture(root: &Path) -> (PathBuf, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let source = root.join("movie.mkv");
    std::fs::write(&source, b"fake video bytes").unwrap();
    (
        source,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    )
}

/// 手动 poll 一次：确定性推进到「已加入同一 in-flight 任务」的等待点。
///
/// 不依赖 sleep：`deliver` 的第一次 poll 会同步完成 fast-path join 并停在
/// `waiter.wait()`，因此这里返回 Pending 就证明第二个请求已经共享同一任务。
async fn poll_once_joined<F: std::future::Future>(future: std::pin::Pin<&mut F>) {
    let mut future = future;
    std::future::poll_fn(|cx| {
        assert!(
            matches!(future.as_mut().poll(cx), Poll::Pending),
            "第一次 poll 应当停在等待共享结果上（而不是已经完成或 panic）"
        );
        Poll::Ready(())
    })
    .await;
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + timeout;
    while !condition() {
        assert!(Instant::now() < deadline, "等待超时：{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_identical_requests_run_once_and_survivor_gets_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, calls, finished) = fixture(tmp.path());
    let gate = Arc::new(Gate::new());
    let runner = Arc::new(FakeRunner::new(
        Some(gate.clone()),
        b"SUBTITLE-BYTES",
        calls.clone(),
        finished.clone(),
        None,
    ));
    let (state, service) = make_state(tmp.path(), runner, 2);
    let ledger = LedgerId::new();

    let first = tokio::spawn({
        let service = service.clone();
        let state = state.clone();
        let request = request(ledger, &source);
        async move { service.deliver(&state, request).await }
    });
    gate.wait_entered();

    // 第二个请求由测试自己推进到 join 完成（确定性，不 sleep）。
    let mut second = Box::pin(service.deliver(&state, request(ledger, &source)));
    poll_once_joined(second.as_mut()).await;

    first.abort();
    gate.release();

    let payload = second.await.unwrap();
    assert_eq!(payload.bytes, b"SUBTITLE-BYTES");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "identical requests must share one extraction"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_waiters_cancelled_cancels_job_and_cleans_staging() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, calls, finished) = fixture(tmp.path());
    let gate = Arc::new(Gate::new());
    let runner = Arc::new(FakeRunner::new(
        Some(gate.clone()),
        b"SHOULD-NOT-PUBLISH",
        calls.clone(),
        finished.clone(),
        None,
    ));
    let (state, service) = make_state(tmp.path(), runner, 2);
    let ledger = LedgerId::new();
    let dir = cache_dir(ledger, &source);

    let first = tokio::spawn({
        let service = service.clone();
        let state = state.clone();
        let request = request(ledger, &source);
        async move { service.deliver(&state, request).await }
    });
    gate.wait_entered();
    let mut second = Box::pin(service.deliver(&state, request(ledger, &source)));
    poll_once_joined(second.as_mut()).await;

    first.abort();
    // 最后一个 waiter 离开才取消任务：直接 drop 第二个请求是确定性的。
    drop(second);
    gate.release();

    // 先等提取真的结束过，再断言没有残留——否则 runner 还没写 staging 时
    // 「目录为空」会假通过。
    wait_until(
        Duration::from_secs(5),
        || finished.load(Ordering::SeqCst) == 1,
        "fake runner 应当返回一次",
    );
    wait_until(
        Duration::from_secs(5),
        || entries(&dir).is_empty(),
        "取消后不得留下 staging/final",
    );
    assert!(!dir.join("sub_2.vtt").exists());
}

#[tokio::test]
async fn warm_cache_does_not_run_runner() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, calls, finished) = fixture(tmp.path());
    let ledger = LedgerId::new();
    let dir = cache_dir(ledger, &source);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("sub_2.vtt"), b"WARM-CACHE-BYTES").unwrap();

    let runner = Arc::new(FakeRunner::new(
        None,
        b"COLD",
        calls.clone(),
        finished.clone(),
        None,
    ));
    let (state, service) = make_state(tmp.path(), runner, 2);

    let payload = service
        .deliver(&state, request(ledger, &source))
        .await
        .unwrap();
    assert_eq!(payload.content_type, "text/vtt; charset=utf-8");
    assert_eq!(payload.bytes, b"WARM-CACHE-BYTES");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "warm cache must not run ffmpeg"
    );
}

/// R3 回归：key 捕获之后源文件被替换，不得返回旧版本的暖缓存。
///
/// current_thread runtime 下第一次 poll 只把 worker 入队（尚未执行），因此
/// 在继续之前替换源文件是确定性的，不依赖调度运气。旧实现只在 `publish_file`
/// 校验版本，暖缓存命中会直接返回旧 bytes。
#[tokio::test]
async fn source_replaced_after_key_capture_does_not_return_stale_warm_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, calls, finished) = fixture(tmp.path());
    let ledger = LedgerId::new();
    let dir = cache_dir(ledger, &source);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("sub_2.vtt"), b"OLD-VERSION-A-SUBTITLE").unwrap();

    let runner = Arc::new(FakeRunner::new(
        None,
        b"COLD",
        calls.clone(),
        finished.clone(),
        None,
    ));
    let (state, service) = make_state(tmp.path(), runner, 2);
    let version_a = api::fingerprint_job::current_source_version(&source);

    let mut delivery = Box::pin(service.deliver(&state, request(ledger, &source)));
    std::future::poll_fn(|cx| {
        assert!(matches!(delivery.as_mut().poll(cx), Poll::Pending));
        Poll::Ready(())
    })
    .await;

    std::fs::write(&source, b"version-B-with-a-different-length").unwrap();
    assert_ne!(
        version_a,
        api::fingerprint_job::current_source_version(&source),
        "替换必须真的改变 source_version"
    );

    let result = delivery.await;
    assert!(
        matches!(result, Err(SubtitleServiceError::Cancelled)),
        "源文件已变时必须取消，而不是返回旧暖缓存: {result:?}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "暖缓存路径不应触发提取"
    );
}

#[tokio::test]
async fn source_version_change_before_publish_does_not_publish_late_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, calls, finished) = fixture(tmp.path());
    let ledger = LedgerId::new();
    let dir = cache_dir(ledger, &source);

    let runner = Arc::new(FakeRunner::new(
        None,
        b"STALE-BYTES",
        calls.clone(),
        finished.clone(),
        Some(source.clone()),
    ));
    let (state, service) = make_state(tmp.path(), runner, 2);

    let result = service.deliver(&state, request(ledger, &source)).await;
    assert!(
        matches!(result, Err(SubtitleServiceError::Cancelled)),
        "source replacement must cancel instead of publishing: {result:?}"
    );
    assert!(
        !dir.join("sub_2.vtt").exists(),
        "no late cache may be published"
    );
    assert!(
        entries(&dir).is_empty(),
        "staging must be removed: {:?}",
        entries(&dir)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn different_keys_exhaust_concurrency_and_report_busy() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, calls, finished) = fixture(tmp.path());
    let gate = Arc::new(Gate::new());
    let runner = Arc::new(FakeRunner::new(
        Some(gate.clone()),
        b"FIRST",
        calls.clone(),
        finished.clone(),
        None,
    ));
    let (state, service) = make_state(tmp.path(), runner, 1);

    let first = tokio::spawn({
        let service = service.clone();
        let state = state.clone();
        let request = request(LedgerId::new(), &source);
        async move { service.deliver(&state, request).await }
    });
    gate.wait_entered();

    let second = service
        .deliver(&state, request(LedgerId::new(), &source))
        .await;
    assert!(
        matches!(second, Err(SubtitleServiceError::Busy)),
        "concurrency limit must report Busy: {second:?}"
    );

    gate.release();
    assert!(first.await.unwrap().is_ok());
}
