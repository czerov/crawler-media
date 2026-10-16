//! P2-NEW-3：投影构建必须在请求预算内、有共享容量上限，并把容量不足/超时
//! 映射为 503/504，而不是静默回退到昂贵的全量读取。
//!
//! 覆盖：
//! - 构建容量饱和 → 目标 Library 的浏览请求立刻 503（不挂起等 30 秒）；
//! - leader 构建超过 deadline → 504（而不是无界等待）；
//! - P3：终态已发布、inflight 条目尚未移除时进入的 follower，必须重放真实终态
//!   （`Ready` 含紧随其后的新脏标记、`Failed`、`StaleScope`），而不是伪造
//!   `Cancelled`/`Timeout`。

#[path = "support/mod.rs"]
mod support;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use api::ApiState;
use api::browse_file_read::BrowseFileRead;
use api::browse_projection::{ProjectionError, ProjectionService};
use api::router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::MediaKind;
use serde_json::Value;
use support::library_presence::PresenceFixture;
use tower::ServiceExt;

/// 第一次 `metadata` 通知测试并阻塞，直到测试放行：用来占住唯一构建 permit。
struct GatedFiles {
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl BrowseFileRead for GatedFiles {
    fn read_nfo(&self, path: &Path) -> Option<library::NfoMeta> {
        library::read_nfo(path)
    }

    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        if let Some(tx) = self.started.lock().unwrap().take() {
            let _ = tx.send(());
        }
        if let Some(rx) = self.release.lock().unwrap().take() {
            let _ = rx.recv();
        }
        std::fs::metadata(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
}

/// 每次 `metadata` 都睡过短 deadline，用来触发 leader 超时。
struct SleepyFiles(Duration);

impl BrowseFileRead for SleepyFiles {
    fn read_nfo(&self, path: &Path) -> Option<library::NfoMeta> {
        library::read_nfo(path)
    }

    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        std::thread::sleep(self.0);
        std::fs::metadata(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
}

async fn get(state: &ApiState, url: &str, token: &str) -> (StatusCode, Value) {
    let app = router(state.clone());
    let request = Request::builder()
        .method("GET")
        .uri(url)
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn second_library(fix: &PresenceFixture) -> String {
    let other_root = fix.dir.path().join("other-movies");
    std::fs::create_dir_all(&other_root).unwrap();
    let store = fix.state.store();
    let guard = store.lock();
    guard
        .create_library(
            MediaKind::Movie,
            "Other",
            &[other_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap()
        .id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saturated_projection_build_capacity_returns_503() {
    let fix = PresenceFixture::new();
    fix.insert_media_and_ledger("Gated.mkv");
    let other_id = second_library(&fix);

    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let files = Arc::new(GatedFiles {
        started: Mutex::new(Some(started_tx)),
        release: Mutex::new(Some(release_rx)),
    });
    let state = fix
        .state
        .clone()
        .with_browse_file_read(files)
        .with_projection_service(Arc::new(ProjectionService::new().with_build_capacity(1)));

    // leader：占住唯一 permit，阻塞在文件 stat。
    let leader_state = state.clone();
    let leader_lib = fix.library_id.clone();
    let leader = tokio::spawn(async move {
        leader_state
            .projection
            .ensure(leader_state.clone(), leader_lib)
            .await
    });
    started_rx.await.expect("leader build must reach file IO");

    // 另一个 Library 的 leader：容量不足必须立刻 503，而不是排队等 deadline。
    let (status, body) = get(
        &state,
        &format!("/api/v1/libraries/{other_id}/items"),
        &fix.admin_token,
    )
    .await;

    // 先放行 leader，避免断言失败时留下永久阻塞的构建线程。
    release_tx.send(()).unwrap();
    let leader_result = leader.await.unwrap();
    assert!(leader_result.is_ok(), "放行后 leader 必须成功发布投影");

    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "容量饱和必须 503：{body}"
    );
    assert_eq!(body["error"]["code"], "runtime.busy");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn leader_build_over_deadline_returns_504() {
    let fix = PresenceFixture::new();
    fix.insert_media_and_ledger("Slow.mkv");
    let state = fix
        .state
        .clone()
        .with_browse_file_read(Arc::new(SleepyFiles(Duration::from_millis(300))))
        .with_projection_service(Arc::new(
            ProjectionService::new()
                .with_build_capacity(2)
                .with_deadline(Duration::from_millis(50)),
        ));

    let (status, body) = get(
        &state,
        &format!("/api/v1/libraries/{}/items", fix.library_id),
        &fix.admin_token,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "leader 构建超过 deadline 必须 504：{body}"
    );
    assert_eq!(body["error"]["code"], "library.timeout");
}

/// 第一次 `metadata` 阻塞并放行，同时记录并发构建重叠数：第二个构建若在第一个
/// 仍阻塞时启动，`max_active` 会变成 2。
struct SingleFlightProbe {
    started: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    active: AtomicUsize,
    max_active: AtomicUsize,
}

impl SingleFlightProbe {
    fn new(
        started: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) -> Self {
        Self {
            started: Mutex::new(Some(started)),
            release: Mutex::new(Some(release)),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        }
    }

    fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }
}

impl BrowseFileRead for SingleFlightProbe {
    fn read_nfo(&self, path: &Path) -> Option<library::NfoMeta> {
        library::read_nfo(path)
    }

    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        if let Some(tx) = self.started.lock().unwrap().take() {
            let _ = tx.send(());
        }
        if let Some(rx) = self.release.lock().unwrap().take() {
            let _ = rx.recv();
        }
        let result = std::fs::metadata(path);
        self.active.fetch_sub(1, Ordering::SeqCst);
        result
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
}

/// P2-C：请求超时只结束等待。同一 Library 的 inflight 条目必须活到 blocking
/// 构建真正结束，因此超时重试只会加入在飞构建（再次 Timeout），不会各起一次
/// 构建、把两个构建槽位都占满后让第三次请求报 Busy。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timed_out_build_keeps_single_flight_until_it_finishes() {
    let fix = PresenceFixture::new();
    fix.insert_media_and_ledger("Gated.mkv");
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let probe = Arc::new(SingleFlightProbe::new(started_tx, release_rx));
    let state = fix
        .state
        .clone()
        .with_browse_file_read(probe.clone())
        .with_projection_service(Arc::new(
            ProjectionService::new()
                .with_build_capacity(2)
                .with_deadline(Duration::from_millis(80)),
        ));

    // leader：占住构建线程，阻塞在文件 stat。
    let leader_state = state.clone();
    let leader_lib = fix.library_id.clone();
    let leader = tokio::spawn(async move {
        leader_state
            .projection
            .ensure(leader_state.clone(), leader_lib)
            .await
    });
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("leader build must reach file IO");

    // 两次超时重试：必须加入在飞构建，而不是各自新起一个构建。
    let second = state
        .projection
        .ensure(state.clone(), fix.library_id.clone())
        .await;
    let third = state
        .projection
        .ensure(state.clone(), fix.library_id.clone())
        .await;
    assert!(
        matches!(second, Err(ProjectionError::Timeout)),
        "第二次必须等待在飞构建并超时，而不是另起构建：{second:?}"
    );
    assert!(
        matches!(third, Err(ProjectionError::Timeout)),
        "第三次不得另起构建（否则会 Busy）：{third:?}"
    );
    assert_eq!(probe.max_active(), 1, "同一 Library 不得并发跑两个构建");
    assert!(
        state
            .store()
            .lock()
            .browse_projection(&fix.library_id)
            .unwrap()
            .is_none(),
        "构建仍被阻塞时不得已发布投影"
    );

    // 放行：blocking 构建跑完并发布，inflight 条目此时才移除。
    release_tx.send(()).unwrap();
    let leader_result = leader.await.unwrap();
    assert!(
        matches!(leader_result, Err(ProjectionError::Timeout)),
        "leader 的请求等待已经超时：{leader_result:?}"
    );

    let mut ready = false;
    for _ in 0..40 {
        if state
            .projection
            .ensure(state.clone(), fix.library_id.clone())
            .await
            .is_ok()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(ready, "构建真正结束后重试必须拿到已发布投影");
    assert!(
        state
            .store()
            .lock()
            .browse_projection(&fix.library_id)
            .unwrap()
            .is_some(),
        "构建完成后投影必须已发布"
    );
}

/// 单飞只约束同一 Library：一个构建阻塞时，另一个 Library 仍能占用第二个槽位。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_different_library_still_builds_while_one_is_blocked() {
    let fix = PresenceFixture::new();
    fix.insert_media_and_ledger("Gated.mkv");
    let other_id = second_library(&fix);
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let probe = Arc::new(SingleFlightProbe::new(started_tx, release_rx));
    let state = fix
        .state
        .clone()
        .with_browse_file_read(probe)
        .with_projection_service(Arc::new(
            ProjectionService::new()
                .with_build_capacity(2)
                .with_deadline(Duration::from_secs(5)),
        ));

    let leader_state = state.clone();
    let leader_lib = fix.library_id.clone();
    let leader = tokio::spawn(async move {
        leader_state
            .projection
            .ensure(leader_state.clone(), leader_lib)
            .await
    });
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("first build must start");

    let other = state.projection.ensure(state.clone(), other_id).await;
    assert!(
        other.is_ok(),
        "不同 Library 必须仍能占用第二个构建槽位：{other:?}"
    );

    release_tx.send(()).unwrap();
    let leader_result = leader.await.unwrap();
    assert!(
        leader_result.is_ok(),
        "放行后被阻塞的构建必须成功发布：{leader_result:?}"
    );
}

/// 终态发布屏障：leader 发布终态后通知测试并阻塞，直到测试放行，从而把
/// follower 确定性地放进「终态已发布、inflight 条目尚未移除」的窗口。
struct PublishGate {
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl PublishGate {
    fn new() -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let gate = Arc::new(Self {
            entered: Mutex::new(Some(entered_tx)),
            release: Mutex::new(Some(release_rx)),
        });
        (gate, entered_rx, release_tx)
    }

    fn barrier(self: &Arc<Self>) -> Arc<dyn Fn(&str) + Send + Sync> {
        let gate = self.clone();
        Arc::new(move |_library_id: &str| {
            if let Some(tx) = gate.entered.lock().unwrap().take() {
                let _ = tx.send(());
            }
            if let Some(rx) = gate.release.lock().unwrap().take() {
                let _ = rx.recv();
            }
        })
    }
}

/// 构建在文件 stat 处 panic：走 blocking worker 失败 → `Failed` 终态。
struct PanicFiles;

impl BrowseFileRead for PanicFiles {
    fn read_nfo(&self, path: &Path) -> Option<library::NfoMeta> {
        library::read_nfo(path)
    }

    fn metadata(&self, _path: &Path) -> std::io::Result<std::fs::Metadata> {
        panic!("projection build fault injection");
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
}

/// 在「终态已发布、条目尚未移除」窗口内进入的 follower，必须重放 leader 的
/// `Ready` 终态 —— 即使紧随其后到达的新脏标记让 freshness 复查落空。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_follower_replays_ready_outcome_after_a_new_dirty_mark() {
    let fix = PresenceFixture::new();
    fix.insert_media_and_ledger("Gated.mkv");
    let (gate, entered_rx, release_tx) = PublishGate::new();
    let state = fix.state.clone().with_projection_service(Arc::new(
        ProjectionService::new()
            .with_build_capacity(2)
            .with_deadline(Duration::from_secs(10))
            .with_publish_barrier(gate.barrier()),
    ));

    let leader_state = state.clone();
    let leader_lib = fix.library_id.clone();
    let leader = tokio::spawn(async move {
        leader_state
            .projection
            .ensure(leader_state.clone(), leader_lib)
            .await
    });
    entered_rx.await.expect("leader 必须发布终态并到达屏障");

    // 终态已发布、条目尚未移除：此刻到达的新脏标记让 follower 的 freshness
    // 复查落空，只能靠重放终态拿到结果。
    state.projection.mark_dirty(&fix.library_id);
    assert!(state.projection.is_dirty(&fix.library_id));

    let follower = tokio::time::timeout(
        Duration::from_secs(2),
        state
            .projection
            .ensure(state.clone(), fix.library_id.clone()),
    )
    .await;
    release_tx.send(()).unwrap();

    let follower = follower.expect("follower 不得等 leader 移除条目才拿到结果");
    let snapshot = follower.expect("follower 必须重放 Ready 终态，而不是伪造 Cancelled/Timeout");
    assert_eq!(snapshot.library_id, fix.library_id);
    assert!(leader.await.unwrap().is_ok(), "leader 必须成功发布投影");
}

/// HTTP 面：窗口内进入的 follower 请求必须拿到真实 `Ready` 结果（200），而不是
/// 被伪造的 `Cancelled` 映射成 503 `runtime.busy`。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_follower_http_request_gets_ready_not_spurious_503() {
    let fix = PresenceFixture::new();
    fix.insert_media_and_ledger("Gated.mkv");
    let (gate, entered_rx, release_tx) = PublishGate::new();
    let state = fix.state.clone().with_projection_service(Arc::new(
        ProjectionService::new()
            .with_build_capacity(2)
            .with_deadline(Duration::from_secs(10))
            .with_publish_barrier(gate.barrier()),
    ));

    let leader_state = state.clone();
    let leader_lib = fix.library_id.clone();
    let leader = tokio::spawn(async move {
        leader_state
            .projection
            .ensure(leader_state.clone(), leader_lib)
            .await
    });
    entered_rx.await.expect("leader 必须发布终态并到达屏障");

    state.projection.mark_dirty(&fix.library_id);
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        get(
            &state,
            &format!("/api/v1/libraries/{}/items", fix.library_id),
            &fix.admin_token,
        ),
    )
    .await;
    release_tx.send(()).unwrap();

    let (status, body) = response.expect("HTTP follower 不得等 leader 移除条目才拿到结果");
    assert_eq!(
        status,
        StatusCode::OK,
        "窗口内 follower 必须拿到真实 Ready 结果，而不是伪造 503：{body}"
    );
    assert!(leader.await.unwrap().is_ok(), "leader 必须成功发布投影");
}

/// 构建 panic → `Failed` 终态。窗口内进入的 follower 必须重放该 `Failed`。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_follower_replays_failed_outcome() {
    let fix = PresenceFixture::new();
    fix.insert_media_and_ledger("Boom.mkv");
    let (gate, entered_rx, release_tx) = PublishGate::new();
    let state = fix
        .state
        .clone()
        .with_browse_file_read(Arc::new(PanicFiles))
        .with_projection_service(Arc::new(
            ProjectionService::new()
                .with_build_capacity(2)
                .with_deadline(Duration::from_secs(10))
                .with_publish_barrier(gate.barrier()),
        ));

    let leader_state = state.clone();
    let leader_lib = fix.library_id.clone();
    let leader = tokio::spawn(async move {
        leader_state
            .projection
            .ensure(leader_state.clone(), leader_lib)
            .await
    });
    entered_rx.await.expect("leader 必须以 Failed 终态到达屏障");

    let follower = tokio::time::timeout(
        Duration::from_secs(2),
        state
            .projection
            .ensure(state.clone(), fix.library_id.clone()),
    )
    .await;
    release_tx.send(()).unwrap();

    let follower = follower.expect("follower 不得等 leader 移除条目才拿到结果");
    assert!(
        matches!(follower, Err(ProjectionError::Failed(_))),
        "follower 必须重放 Failed，而不是伪造 Cancelled/Timeout：{follower:?}"
    );
    assert!(
        matches!(leader.await.unwrap(), Err(ProjectionError::Failed(_))),
        "leader 必须回报 Failed"
    );
}

/// 构建期间公共事实变化 → publish 判定过期 → `StaleScope` 终态。窗口内进入的
/// follower 必须重放该 `StaleScope`。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_follower_replays_stale_scope_outcome() {
    let fix = PresenceFixture::new();
    fix.insert_media_and_ledger("Gated.mkv");
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let (build_release_tx, build_release_rx) = std::sync::mpsc::channel::<()>();
    let files = Arc::new(GatedFiles {
        started: Mutex::new(Some(started_tx)),
        release: Mutex::new(Some(build_release_rx)),
    });
    let (gate, entered_rx, gate_release_tx) = PublishGate::new();
    let state = fix
        .state
        .clone()
        .with_browse_file_read(files)
        .with_projection_service(Arc::new(
            ProjectionService::new()
                .with_build_capacity(2)
                .with_deadline(Duration::from_secs(10))
                .with_publish_barrier(gate.barrier()),
        ));

    let leader_state = state.clone();
    let leader_lib = fix.library_id.clone();
    let leader = tokio::spawn(async move {
        leader_state
            .projection
            .ensure(leader_state.clone(), leader_lib)
            .await
    });
    started_rx.await.expect("leader build must reach file IO");

    // 构建已抓取 revision；此刻写公共事实 → publish 判定 scope 已过期。
    fix.insert_media_and_ledger("Extra.mkv");
    build_release_tx.send(()).unwrap();

    entered_rx
        .await
        .expect("leader 必须以 StaleScope 终态到达屏障");

    let follower = tokio::time::timeout(
        Duration::from_secs(2),
        state
            .projection
            .ensure(state.clone(), fix.library_id.clone()),
    )
    .await;
    gate_release_tx.send(()).unwrap();

    let follower = follower.expect("follower 不得等 leader 移除条目才拿到结果");
    assert!(
        matches!(follower, Err(ProjectionError::StaleScope)),
        "follower 必须重放 StaleScope，而不是伪造 Cancelled/Timeout：{follower:?}"
    );
    assert!(
        matches!(leader.await.unwrap(), Err(ProjectionError::StaleScope)),
        "leader 必须回报 StaleScope"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn library_removal_waits_for_blocked_projection_build() {
    let fix = PresenceFixture::new();
    fix.insert_media_and_ledger("Gated.mkv");
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let files = Arc::new(GatedFiles {
        started: Mutex::new(Some(started_tx)),
        release: Mutex::new(Some(release_rx)),
    });
    let state = fix.state.clone().with_browse_file_read(files).with_projection_service(
        Arc::new(ProjectionService::new().with_build_capacity(1)),
    );
    let leader_state = state.clone();
    let leader_library = fix.library_id.clone();
    let leader = tokio::spawn(async move {
        leader_state.projection.ensure(leader_state.clone(), leader_library).await
    });
    started_rx.await.expect("projection build must enter gated metadata read");

    let removal = api::library_lifecycle::start_library_removal(&state, &fix.library_id).unwrap();
    let completed_while_blocked = tokio::time::timeout(Duration::from_millis(100), async {
        loop {
            let current = state.store().lock().library_removal(&removal.operation_id).unwrap().unwrap();
            if current.state == "succeeded" { break; }
            tokio::task::yield_now().await;
        }
    }).await.is_ok();
    release_tx.send(()).unwrap();
    assert!(!completed_while_blocked, "removal must drain the in-flight projection build");
    assert!(leader.await.unwrap().is_err());
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let current = state.store().lock().library_removal(&removal.operation_id).unwrap().unwrap();
            let dependencies_remain = state.projection.has_dependencies(&fix.library_id);
            if current.state == "succeeded" && !dependencies_remain { break; }
            assert_ne!(current.state, "failed", "{:?}", current.error);
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert!(state.store().lock().get_library(&fix.library_id).unwrap().is_none());
    assert!(!state.projection.has_dependencies(&fix.library_id), "deleted Library must have no dependency entries");

    let retry = state.projection.ensure(state.clone(), fix.library_id.clone()).await;
    assert!(retry.is_err(), "deleted Library must not retain a buildable projection");
    assert_eq!(state.projection.available_build_permits(), 1, "completed worker must release capacity");
}
