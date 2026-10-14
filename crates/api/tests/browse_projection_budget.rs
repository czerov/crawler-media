//! P2-NEW-3：投影构建必须在请求预算内、有共享容量上限，并把容量不足/超时
//! 映射为 503/504，而不是静默回退到昂贵的全量读取。
//!
//! 覆盖：
//! - 构建容量饱和 → 目标 Library 的浏览请求立刻 503（不挂起等 30 秒）；
//! - leader 构建超过 deadline → 504（而不是无界等待）。

#[path = "support/mod.rs"]
mod support;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use api::ApiState;
use api::browse_file_read::BrowseFileRead;
use api::browse_projection::ProjectionService;
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
