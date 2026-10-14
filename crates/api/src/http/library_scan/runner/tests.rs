//! Video 扫描路径的 WorkControl 取消测试。
//!
//! 集成测试无法确定性地「在遍历中途」取消（`test_scope_hook` 只在 crate 内可见，
//! 且 `WorkControl` 由 coordinator 在运行时注册）。这里直接驱动 `ScanContext`，
//! 证明 Video 也走同一个可取消 walker：控制被取消时立刻失败、零登记，并把
//! 409 `library.deleting` 语义交回调用方。

use std::sync::Arc;

use axum::http::StatusCode;
use domain::MediaKind;

use crate::management::ApiState;

struct NoFetch;

impl indexer::Fetcher for NoFetch {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch("offline".into()))
    }
}

fn video_fixture() -> (tempfile::TempDir, ApiState, String, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::open(dir.path().join("data")).unwrap();
    let state = ApiState::new(
        store,
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(downloader::MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap();
    let root = dir.path().join("homevideos");
    std::fs::create_dir_all(&root).unwrap();
    let library_id = {
        let store = state.store();
        let guard = store.lock();
        guard
            .create_library(
                MediaKind::Video,
                "Home Videos",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
    };
    (dir, state, library_id, root)
}

/// 取消的 Library work control 必须让 Video chunk 处理在登记之前停下。
#[test]
fn cancelled_video_scan_fails_without_ledgering() {
    let (_dir, state, library_id, root) = video_fixture();
    std::fs::write(root.join("clip.mp4"), b"v").unwrap();

    let lease = state.acquire_library_work(&library_id).unwrap();
    let scan_scope =
        crate::http::library_scan::scope::ScanScope::capture(&state, &library_id).unwrap();
    lease.control().cancel();

    let ctx = super::walk::ScanContext {
        state: &state,
        id: &library_id,
        lease: &lease,
        scan_scope: &scan_scope,
        kind: MediaKind::Video,
        scrape: false,
        op_id: None,
    };
    let failure = ctx
        .walk_and_process(&root, &root)
        .err()
        .expect("取消必须让 Video 遍历失败");
    assert_eq!(
        failure.response.status(),
        StatusCode::CONFLICT,
        "取消沿用 library.deleting 合同"
    );
    assert_eq!(failure.error, "library.deleting");
    assert_eq!(failure.totals.scanned, 0);
    assert_eq!(failure.totals.inserted, 0);
    assert!(
        state.store().lock().list_ledger().unwrap().is_empty(),
        "取消的 Video 扫描不得登记任何文件"
    );
}
