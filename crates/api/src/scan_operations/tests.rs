use std::sync::Arc;

use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use store::ScanOperation;

use super::*;

struct NoFetch;

impl indexer::Fetcher for NoFetch {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch("offline".into()))
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    state: ApiState,
    library_id: String,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        Self::with_kind(MediaKind::Movie)
    }

    fn with_kind(kind: MediaKind) -> Self {
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
        let root = dir.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        let library_id = {
            let store = state.store();
            let guard = store.lock();
            guard
                .create_library(
                    kind,
                    "Movies",
                    &[root.to_str().unwrap()],
                    "everyone",
                    true,
                    &[],
                )
                .unwrap()
                .id
        };
        Self {
            _dir: dir,
            state,
            library_id,
            root,
        }
    }

    fn signature(&self) -> String {
        let store = self.state.store();
        let guard = store.lock();
        let library = guard.get_library(&self.library_id).unwrap().unwrap();
        scan_scope_signature(&library)
    }

    fn insert_media_and_ledger(&self, rel_path: &str) -> (MediaId, LedgerRow) {
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "Test Movie".to_string(),
            year: None,
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        let row = LedgerRow {
            id: LedgerId::new(),
            media_id: media.id,
            path: self.root.join(rel_path).to_str().unwrap().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        };
        let store = self.state.store();
        let guard = store.lock();
        guard.insert_media(&media).unwrap();
        guard.insert_ledger(&row).unwrap();
        (media.id, row)
    }

    async fn wait_terminal(&self, op_id: &str) -> ScanOperation {
        for _ in 0..600 {
            let op = self.state.store().lock().scan_operation(op_id).unwrap();
            if let Some(op) = op
                && !op.status.is_active()
            {
                return op;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("scan operation {op_id} 未在预期时间内进入终态");
    }
}

/// P2-4：scope 指纹必须无歧义——根路径里的逗号不能造成不同配置碰撞。
#[test]
fn scope_signature_does_not_collide_on_commas_in_roots() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::open(dir.path().join("data")).unwrap();
    let a = store
        .create_library(
            MediaKind::Movie,
            "A",
            &["/x/a,/x/b", "/x/c"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let b = store
        .create_library(
            MediaKind::Movie,
            "B",
            &["/x/a", "/x/b,/x/c"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    assert_ne!(
        scan_scope_signature(&a),
        scan_scope_signature(&b),
        "逗号拼接会让不同根配置碰撞: {}",
        scan_scope_signature(&a)
    );
    // kind 也必须参与编码。
    let tv = store
        .create_library(
            MediaKind::Tv,
            "TV",
            &["/x/a,/x/b", "/x/c"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    assert_ne!(scan_scope_signature(&a), scan_scope_signature(&tv));
}

/// P2-A：v1 编码 `kind:count:root1,root2,...` 不是单射——任务给出的两组根数组编码
/// 逐字节相同，因此 `match_legacy_scope_signature` 绝不能把这种相等当成 `Same`。
#[test]
fn colliding_v1_encoding_is_not_proof_of_equality() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::open(dir.path().join("data")).unwrap();
    let old = store
        .create_library(
            MediaKind::Movie,
            "Old",
            &["/x/a,/x/b", "/x/c"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let new = store
        .create_library(
            MediaKind::Movie,
            "New",
            &["/x/a", "/x/b,/x/c"],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let old_legacy = legacy_scope_signature(&old);
    assert_eq!(old_legacy, "movie:2:/x/a,/x/b,/x/c");
    assert_eq!(
        old_legacy,
        legacy_scope_signature(&new),
        "测试前提：两组根数组的 v1 编码逐字节碰撞"
    );
    assert_ne!(
        scan_scope_signature(&old),
        scan_scope_signature(&new),
        "v2 编码必须区分这两组配置（P2-4）"
    );
    assert!(
        !legacy_encoding_is_injective(&new),
        "含逗号的多根 v1 编码不可唯一还原"
    );
    assert_eq!(
        match_legacy_scope_signature(&old_legacy, &new),
        Some(LegacySignatureMatch::Ambiguous),
        "碰撞的遗留签名不能升级成当前配置"
    );
}

/// P2-A：无逗号的多根配置，v1 编码可唯一还原，逐字节相等就是配置相等的证明。
#[test]
fn comma_free_v1_encoding_is_provably_equivalent() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::open(dir.path().join("data")).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "A",
            &["/x/a", "/x/b"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let legacy = legacy_scope_signature(&library);
    assert_eq!(legacy, "movie:2:/x/a,/x/b");
    assert!(legacy_encoding_is_injective(&library));
    assert_eq!(
        match_legacy_scope_signature(&legacy, &library),
        Some(LegacySignatureMatch::Same)
    );
}

/// P2-A：单根时 joined 就是唯一根（没有分隔符），即使路径含逗号也可唯一还原。
#[test]
fn single_root_v1_encoding_is_provably_equivalent_even_with_comma() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::open(dir.path().join("data")).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "A",
            &["/x/a,b"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let legacy = legacy_scope_signature(&library);
    assert_eq!(legacy, "movie:1:/x/a,b");
    assert!(legacy_encoding_is_injective(&library));
    assert_eq!(
        match_legacy_scope_signature(&legacy, &library),
        Some(LegacySignatureMatch::Same)
    );
}

/// P3-1 + P3-3：轮内并入的事件必须让本轮补扫，且终态报告是跨轮累计。
#[tokio::test]
async fn dirty_event_during_round_requeues_with_cumulative_report() {
    let fix = Fixture::new();
    let dir = fix.root.join("Show A");
    std::fs::create_dir_all(&dir).unwrap();
    let f1 = dir.join("Movie1 (2024).mkv");
    std::fs::write(&f1, b"1").unwrap();
    fix.insert_media_and_ledger("Show A/Movie1 (2024).mkv");
    let sig = fix.signature();
    let op = {
        let store = fix.state.store();
        let guard = store.lock();
        let op = guard
            .enqueue_scan(&fix.library_id, &sig, &[dir.clone()], 1000)
            .unwrap();
        guard.record_scan_seen(&op.id, &[f1.clone()]).unwrap();
        op
    };
    let f2 = dir.join("Movie2 (2024).mkv");
    std::fs::write(&f2, b"2").unwrap();

    // 屏障：本轮已经开始（认领后），此时并入同一 target 的第二个事件。
    let state = fix.state.clone();
    let library_id = fix.library_id.clone();
    let target = dir.clone();
    let hook_sig = sig.clone();
    let hook_op_id = op.id.clone();
    crate::http::library_scan::runner::test_scope_hook::set(
        &hook_op_id,
        Box::new(move || {
            let store = state.store();
            let guard = store.lock();
            guard
                .enqueue_scan(&library_id, &hook_sig, &[target.clone()], 1001)
                .unwrap();
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_terminal(&op.id).await;
    assert_eq!(done.status, ScanOperationStatus::Completed, "{:?}", done.report);
    assert_eq!(done.dirty_rev, 1);
    assert_eq!(done.consumed_rev, 1, "轮内事件必须被补扫轮消费");
    assert_eq!(
        done.report["scanned"], 1,
        "累计报告必须包含第一轮已处理的文件: {}",
        done.report
    );
    assert_eq!(done.report["inserted"], 1, "{}", done.report);
    let rows = fix.state.store().lock().list_ledger().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .any(|row| row.path.ends_with("Movie2 (2024).mkv")));
}

/// P2-4：准入通过之后、scope 捕获之前 PATCH roots，旧 operation 必须取消且零登记。
#[tokio::test]
async fn scope_change_between_admission_and_capture_cancels_without_ledgering() {
    let fix = Fixture::new();
    let file = fix.root.join("Movie1 (2024).mkv");
    std::fs::write(&file, b"1").unwrap();
    let sig = fix.signature();
    let op = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .enqueue_scan(&fix.library_id, &sig, &[], 1000)
            .unwrap()
    };

    // 屏障：准入已通过，但在捕获 scope 之前把 roots 换到别处。
    let moved_root = fix.root.parent().unwrap().join("movies-moved");
    std::fs::create_dir_all(&moved_root).unwrap();
    let state = fix.state.clone();
    let library_id = fix.library_id.clone();
    let moved = moved_root.to_str().unwrap().to_string();
    let hook_op_id = op.id.clone();
    crate::http::library_scan::runner::test_scope_hook::set(
        &hook_op_id,
        Box::new(move || {
            let store = state.store();
            let guard = store.lock();
            guard
                .update_library(&library_id, None, Some(&[moved.as_str()]), None)
                .unwrap();
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_terminal(&op.id).await;
    assert_eq!(
        done.status,
        ScanOperationStatus::Cancelled,
        "{:?}",
        done.report
    );
    assert_eq!(done.report["error"], "library.scope_changed");
    assert!(
        done.report["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("captured scope changed")),
        "取消原因必须来自捕获 scope 复核: {}",
        done.report
    );
    assert!(
        fix.state.store().lock().list_ledger().unwrap().is_empty(),
        "取消的 operation 不得登记任何文件"
    );
}

/// P2-NEW-2：文件版本观察（模拟慢 NAS stat）期间不得持有 Store 锁。
///
/// 屏障阻塞在观察点；测试线程必须仍能立刻拿到 Store 锁，否则说明慢观察把共享
/// Store 拖住了（auth 等其他请求会被阻塞）。
#[tokio::test]
async fn slow_file_observation_keeps_the_store_lock_free() {
    let fix = Fixture::new();
    let file = fix.root.join("Movie1 (2024).mkv");
    std::fs::write(&file, b"1").unwrap();
    let sig = fix.signature();
    let op = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .enqueue_scan(&fix.library_id, &sig, &[], 1000)
            .unwrap()
    };

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let hook_op_id = op.id.clone();
    crate::http::library_scan::runner::test_observe_hook::set(
        &hook_op_id,
        Box::new(move || {
            let _ = entered_tx.send(());
            // 模拟慢 NAS stat：观察窗口内一直阻塞。
            let _ = release_rx.recv();
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    entered_rx.await.expect("worker 必须进入慢观察屏障");

    let store = fix.state.store();
    let guard = store
        .try_lock_for(std::time::Duration::from_secs(2))
        .expect("慢文件观察期间 Store 锁必须可用（P2-NEW-2）");
    assert!(
        guard.scan_operation(&op.id).unwrap().is_some(),
        "慢观察期间其他 Store 使用者必须仍能读"
    );
    drop(guard);
    release_tx.send(()).unwrap();

    let done = fix.wait_terminal(&op.id).await;
    assert_eq!(done.status, ScanOperationStatus::Completed, "{:?}", done.report);
    assert_eq!(done.report["scanned"], 1, "{}", done.report);
}

/// P3-NEW-1：补扫轮把状态标记为 Running 时，requeue 事务刚落库的累计报告必须还在。
#[tokio::test]
async fn running_transition_keeps_requeued_cumulative_report() {
    let fix = Fixture::new();
    let dir = fix.root.join("Show A");
    std::fs::create_dir_all(&dir).unwrap();
    let f1 = dir.join("Movie1 (2024).mkv");
    std::fs::write(&f1, b"1").unwrap();
    fix.insert_media_and_ledger("Show A/Movie1 (2024).mkv");
    let sig = fix.signature();
    let op = {
        let store = fix.state.store();
        let guard = store.lock();
        let op = guard
            .enqueue_scan(&fix.library_id, &sig, &[dir.clone()], 1000)
            .unwrap();
        guard.record_scan_seen(&op.id, &[f1.clone()]).unwrap();
        op
    };
    let f2 = dir.join("Movie2 (2024).mkv");
    std::fs::write(&f2, b"2").unwrap();

    // 第二轮（补扫轮）在 Running 写入之后、捕获 scope 之前读一次持久化报告。
    let observed: Arc<std::sync::Mutex<Option<serde_json::Value>>> =
        Arc::new(std::sync::Mutex::new(None));
    let state = fix.state.clone();
    let library_id = fix.library_id.clone();
    let target = dir.clone();
    let hook_sig = sig.clone();
    let hook_key = op.id.clone();
    let op_id = op.id.clone();
    let observed_round2 = observed.clone();
    crate::http::library_scan::runner::test_scope_hook::set(
        &hook_key,
        Box::new(move || {
            // 第一轮：并入第二个事件 → 本轮补扫重排并落库累计报告。
            {
                let store = state.store();
                let guard = store.lock();
                guard
                    .enqueue_scan(&library_id, &hook_sig, &[target.clone()], 1001)
                    .unwrap();
            }
            let observed = observed_round2.clone();
            let state_round2 = state.clone();
            let round2_key = op_id.clone();
            let op_id_round2 = op_id.clone();
            crate::http::library_scan::runner::test_scope_hook::set(
                &round2_key,
                Box::new(move || {
                    let report = state_round2
                        .store()
                        .lock()
                        .scan_operation(&op_id_round2)
                        .unwrap()
                        .map(|op| op.report);
                    *observed.lock().unwrap() = report;
                }),
            );
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_terminal(&op.id).await;
    assert_eq!(done.status, ScanOperationStatus::Completed, "{:?}", done.report);
    assert_eq!(done.dirty_rev, 1);
    let seen = observed
        .lock()
        .unwrap()
        .clone()
        .expect("第二轮必须触发观察钩子");
    assert_eq!(
        seen["scanned"], 1,
        "补扫轮标记 Running 不得清空 requeue 落库的累计报告: {seen}"
    );
    assert_eq!(seen["inserted"], 1, "{seen}");
}

/// P3-NEW-2：失败轮因 dirty 事件补扫后成功，失败记录与 HTTP 状态必须保留。
#[tokio::test]
async fn requeued_failed_round_keeps_bounded_failure_history() {
    let fix = Fixture::new();
    let file = fix.root.join("Movie1 (2024).mkv");
    std::fs::write(&file, b"1").unwrap();
    let sig = fix.signature();
    let op = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .enqueue_scan(&fix.library_id, &sig, &[], 1000)
            .unwrap()
    };

    // 第一轮：元数据阶段注入失败（ledger 已提交），同时并入第二个事件触发补扫。
    let state = fix.state.clone();
    let library_id = fix.library_id.clone();
    let hook_sig = sig.clone();
    let op_id = op.id.clone();
    crate::http::library_scan::runner::test_scope_hook::set(
        &op_id,
        Box::new(move || {
            let store = state.store();
            let guard = store.lock();
            guard
                .enqueue_scan(&library_id, &hook_sig, &[], 1001)
                .unwrap();
        }),
    );
    crate::http::library_scan::runner::test_metadata_hook::set(
        &op.id,
        Box::new(|| {
            Some(crate::http::err(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "library.scan_metadata",
                "injected failure",
            ))
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_terminal(&op.id).await;
    assert_eq!(done.status, ScanOperationStatus::Completed, "{:?}", done.report);
    assert_eq!(
        done.report["failure_count"], 1,
        "失败次数必须跨轮累计: {}",
        done.report
    );
    let failures = done.report["failures"]
        .as_array()
        .unwrap_or_else(|| panic!("失败历史必须保留: {}", done.report));
    assert_eq!(failures[0]["error"], "library.scan_metadata", "{}", done.report);
    assert_eq!(failures[0]["http_status"], 500, "{}", done.report);
    assert_eq!(
        done.report["last_failure"]["http_status"], 500,
        "最近一次失败（含 HTTP 状态）必须可读: {}",
        done.report
    );
    // 第一轮已提交的前缀 + 第二轮的成功轮，都要在累计计数里。
    assert_eq!(done.report["scanned"], 2, "{}", done.report);
    assert_eq!(done.report["inserted"], 1, "{}", done.report);
    assert_eq!(fix.state.store().lock().list_ledger().unwrap().len(), 1);
}

/// P3-3：chunk 已提交 ledger 后失败，失败报告必须带上该 chunk 的已提交前缀。
#[tokio::test]
async fn chunk_failure_after_ledger_commit_reports_its_prefix() {
    let fix = Fixture::new();
    let file = fix.root.join("Movie1 (2024).mkv");
    std::fs::write(&file, b"1").unwrap();
    let sig = fix.signature();
    let op = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .enqueue_scan(&fix.library_id, &sig, &[], 1000)
            .unwrap()
    };
    crate::http::library_scan::runner::test_metadata_hook::set(
        &op.id,
        Box::new(|| {
            Some(crate::http::err(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "library.scan_metadata",
                "injected failure",
            ))
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_terminal(&op.id).await;
    assert_eq!(done.status, ScanOperationStatus::Failed, "{:?}", done.report);
    assert_eq!(
        done.report["scanned"], 1,
        "失败 chunk 已提交的 scanned 前缀必须计入: {}",
        done.report
    );
    assert_eq!(
        done.report["inserted"], 1,
        "失败 chunk 已提交的 inserted 前缀必须计入: {}",
        done.report
    );
    assert!(
        done.report["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()),
        "{}",
        done.report
    );
    assert_eq!(
        fix.state.store().lock().list_ledger().unwrap().len(),
        1,
        "ledger 已提交，报告必须承认这段前缀"
    );
}

/// P2-4(a)：operation 读不到/不存在时必须失败关闭，而不是绕过捕获 scope 复核。
#[test]
fn missing_operation_fails_closed_without_ledgering() {
    let fix = Fixture::new();
    let file = fix.root.join("Movie1 (2024).mkv");
    std::fs::write(&file, b"1").unwrap();
    let lease = fix.state.acquire_library_work(&fix.library_id).unwrap();

    let result = crate::http::library_scan::scan_library_leased_resumable(
        &fix.state,
        &fix.library_id,
        &lease,
        &[],
        true,
        Some("missing-operation-id"),
    );
    match result {
        Err(crate::http::library_scan::runner::ScanRunError::Failed(failure)) => {
            assert_eq!(failure.response.status(), axum::http::StatusCode::CONFLICT);
            assert_eq!(failure.error, "library.scan_operation_missing");
        }
        Err(crate::http::library_scan::runner::ScanRunError::ScopeChanged { reason }) => {
            panic!("缺失 operation 不得当成 scope 变化: {reason}")
        }
        Ok(_) => panic!("无法对账的 operation 不得放行"),
    }
    assert!(
        fix.state.store().lock().list_ledger().unwrap().is_empty(),
        "失败关闭时不得登记任何文件"
    );
}

/// P2-4(b)：Video 也必须在发布前把捕获 scope 绑定到持久化 signature。
#[tokio::test]
async fn video_scope_change_between_admission_and_capture_cancels_without_ledgering() {
    let fix = Fixture::with_kind(MediaKind::Video);
    let file = fix.root.join("clip.mp4");
    std::fs::write(&file, b"1").unwrap();
    let sig = fix.signature();
    let op = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .enqueue_scan(&fix.library_id, &sig, &[], 1000)
            .unwrap()
    };

    let moved_root = fix.root.parent().unwrap().join("movies-moved");
    std::fs::create_dir_all(&moved_root).unwrap();
    // 新根里有可入账的文件：如果 Video 绕过复核，就会把它登记进台账。
    std::fs::write(moved_root.join("other.mp4"), b"x").unwrap();
    let state = fix.state.clone();
    let library_id = fix.library_id.clone();
    let moved = moved_root.to_str().unwrap().to_string();
    let hook_op_id = op.id.clone();
    crate::http::library_scan::runner::test_scope_hook::set(
        &hook_op_id,
        Box::new(move || {
            let store = state.store();
            let guard = store.lock();
            guard
                .update_library(&library_id, None, Some(&[moved.as_str()]), None)
                .unwrap();
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_terminal(&op.id).await;
    assert_eq!(
        done.status,
        ScanOperationStatus::Cancelled,
        "Video 捕获 scope 变化必须取消: {:?}",
        done.report
    );
    assert!(
        done.report["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("captured scope changed")),
        "取消原因必须来自捕获 scope 复核: {}",
        done.report
    );
    assert!(
        fix.state.store().lock().list_ledger().unwrap().is_empty(),
        "取消的 Video operation 不得登记任何文件"
    );
}
