#[path = "support/mod.rs"]
mod support;

use serde_json::Value;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn scan_seen_checkpoints_and_avoids_reprocessing() {
    let fix = PresenceFixture::new();

    let f1 = fix.root.join("Movie1 (2024).mkv");
    let f2 = fix.root.join("Movie2 (2024).mkv");
    std::fs::write(&f1, b"1").unwrap();
    std::fs::write(&f2, b"2").unwrap();

    let store_arc = fix.state.store();
    let op = {
        let guard = store_arc.lock();
        guard.enqueue_scan(&fix.library_id, "sig", &[], 1000).unwrap()
    };

    // Checkpoint f1 as seen
    {
        let guard = store_arc.lock();
        guard.record_scan_seen(&op.id, &[f1.clone()]).unwrap();
        assert!(guard.is_scan_seen(&op.id, &f1.display().to_string()).unwrap());
        assert!(!guard.is_scan_seen(&op.id, &f2.display().to_string()).unwrap());
    }

    // Clear seen
    {
        let guard = store_arc.lock();
        guard.clear_scan_seen(&op.id).unwrap();
        assert!(!guard.is_scan_seen(&op.id, &f1.display().to_string()).unwrap());
    }
}

/// 轮询 store 直到 operation 进入终态，返回落库的 report。
async fn wait_scan_terminal(fix: &PresenceFixture, op_id: &str) -> Value {
    for _ in 0..200 {
        let op = fix.state.store().lock().scan_operation(op_id).unwrap();
        if let Some(op) = op
            && !op.status.is_active()
        {
            return op.report;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("scan operation {op_id} 未在预期时间内进入终态");
}

/// 中断/重启后，同一个 scan operation 必须从 `scan_seen` 断点续扫：已处理过的文件
/// 不再重新入账/探测，只处理新增文件（B3 §4.2）。
#[tokio::test]
async fn resumed_scan_skips_checkpointed_files() {
    let fix = PresenceFixture::new();
    let f1 = fix.root.join("Movie1 (2024).mkv");
    std::fs::write(&f1, b"1").unwrap();
    let sig = fix.scope_signature();

    // 第一轮：真实扫完 f1（入账 + 探测入队）。
    let op_id = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .enqueue_scan(&fix.library_id, &sig, &[], 1000)
            .unwrap()
            .id
    };
    fix.state.scan_coordinator.trigger_next(&fix.state);
    wait_scan_terminal(&fix, &op_id).await;
    let first = fix.state.store().lock().list_ledger().unwrap();
    assert_eq!(first.len(), 1, "第一轮应入账 f1");
    let f1_row = first[0].id.to_string();
    // 让 f1 的探测结束，这样“断点生效”不会被“本来就没再探测”混淆。
    {
        let store = fix.state.store();
        let job_id = store
            .lock()
            .active_probe_unit_for_ledger(&f1_row)
            .unwrap()
            .expect("f1 应有一个探测单元")
            .job_id;
        store
            .lock()
            .finish_probe_unit(&job_id, &f1_row, true, None)
            .unwrap();
        store.lock().finish_probe_job(&job_id, true, None).unwrap();
        assert!(!fix.state.probe.is_queued(&f1_row));
    }

    // 模拟中断重启：新 operation 的第一轮已经处理并记下 f1，随后进程崩溃，
    // recover_orphaned_scans 把 running 的 operation 排回队列（断点保留）。
    let f2 = fix.root.join("Movie2 (2024).mkv");
    std::fs::write(&f2, b"2").unwrap();
    let op2_id = {
        let store = fix.state.store();
        let guard = store.lock();
        let op = guard
            .enqueue_scan(&fix.library_id, &sig, &[], 2000)
            .unwrap();
        assert_ne!(op.id, op_id, "上一个 operation 已终态，应新建 operation");
        guard.record_scan_seen(&op.id, &[f1.clone()]).unwrap();
        guard.claim_next_queued_scan(&[]).unwrap().expect("operation 应可被认领");
        assert_eq!(guard.recover_orphaned_scans().unwrap(), 1, "running 的 operation 应被排回队列");
        op.id
    };

    // 续扫：f1 命中断点不再处理，只有新增的 f2 入账并进入探测队列。
    fix.state.scan_coordinator.trigger_next(&fix.state);
    let report = wait_scan_terminal(&fix, &op2_id).await;
    assert_eq!(
        report["scanned"], 1,
        "断点命中的 f1 不得被重复处理: {report}"
    );
    assert_eq!(report["inserted"], 1, "只有新增的 f2 入账: {report}");
    let rows = fix.state.store().lock().list_ledger().unwrap();
    assert_eq!(rows.len(), 2);
    let f2_row = rows
        .iter()
        .find(|row| row.path.ends_with("Movie2 (2024).mkv"))
        .expect("f2 应已入账")
        .id
        .to_string();
    assert!(fix.state.probe.is_queued(&f2_row), "新增文件必须进入探测队列");
    assert!(
        !fix.state.probe.is_queued(&f1_row),
        "断点命中的 f1 不得被重新探测"
    );
}

/// P2-3 端到端：文件被删除后恢复，预置的断点不能让它被跳过；missing 必须清掉。
#[tokio::test]
async fn restored_file_is_reprocessed_and_missing_cleared() {
    let fix = PresenceFixture::new();
    let rel = "Movie1 (2024).mkv";
    let file = fix.root.join(rel);
    // 台账先建、文件后到：verify 会把缺失标记写上（复现 P2-3 的前置状态）。
    fix.insert_media_and_ledger(rel);
    {
        let store = fix.state.store();
        let guard = store.lock();
        let (missing, _restored) = guard
            .verify_library_files(std::slice::from_ref(&fix.root), 1)
            .unwrap();
        assert_eq!(missing, 1, "文件缺失时应标记 missing");
        assert!(
            guard
                .missing_at_by_path()
                .unwrap()
                .contains_key(&file.display().to_string()),
            "前置状态必须带 missing 标记"
        );
    }
    // 文件恢复（内容与缺失前不同也无所谓，关键是路径已存在）。
    std::fs::write(&file, b"restored").unwrap();

    let sig = fix.scope_signature();
    let op = {
        let store = fix.state.store();
        let guard = store.lock();
        let op = guard
            .enqueue_scan(&fix.library_id, &sig, &[], 1000)
            .unwrap();
        // 预置断点：路径「已经扫过」，但台账仍是 missing。
        guard.record_scan_seen(&op.id, &[file.clone()]).unwrap();
        op
    };
    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_scan_terminal_op(&op.id).await;
    assert_eq!(done.status, store::ScanOperationStatus::Completed, "{:?}", done.report);
    assert_eq!(
        done.report["scanned"], 1,
        "恢复的文件必须重新处理而不是命中断点跳过: {}",
        done.report
    );

    let store = fix.state.store();
    let guard = store.lock();
    assert!(
        guard.missing_at_by_path().unwrap().is_empty(),
        "恢复后 missing 标记必须清掉"
    );
}

/// P2-3 端到端：内容变化的文件即使路径已进断点，也必须重新处理。
#[tokio::test]
async fn changed_file_is_reprocessed_despite_checkpoint() {
    let fix = PresenceFixture::new();
    let rel = "Movie1 (2024).mkv";
    let file = fix.root.join(rel);
    std::fs::write(&file, b"v1").unwrap();
    fix.insert_media_and_ledger(rel);
    let sig = fix.scope_signature();

    let op = {
        let store = fix.state.store();
        let guard = store.lock();
        let op = guard
            .enqueue_scan(&fix.library_id, &sig, &[], 1000)
            .unwrap();
        guard.record_scan_seen(&op.id, &[file.clone()]).unwrap();
        op
    };
    // 内容变化（size 变）：断点必须失效。
    std::fs::write(&file, b"v1-changed-content").unwrap();

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_scan_terminal_op(&op.id).await;
    assert_eq!(done.status, store::ScanOperationStatus::Completed, "{:?}", done.report);
    assert_eq!(
        done.report["scanned"], 1,
        "内容变化的文件必须重新处理: {}",
        done.report
    );
}

/// P2-4 端到端：持久化 scope signature 与当前 scope 不一致的 operation 必须取消，
/// 且不得登记任何文件。
#[tokio::test]
async fn stale_scope_signature_cancels_operation_without_ledgering() {
    let fix = PresenceFixture::new();
    let file = fix.root.join("Movie1 (2024).mkv");
    std::fs::write(&file, b"1").unwrap();

    let op_id = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .enqueue_scan(&fix.library_id, "definitely-old-scope", &[], 1000)
            .unwrap()
            .id
    };
    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_scan_terminal_op(&op_id).await;
    assert_eq!(
        done.status,
        store::ScanOperationStatus::Cancelled,
        "过期 scope 必须取消: {}",
        done.report
    );
    assert_eq!(done.report["error"], "library.scope_changed");
    assert!(
        done.report["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("definitely-old-scope")),
        "取消原因必须写进报告: {}",
        done.report
    );
    assert!(
        fix.state.store().lock().list_ledger().unwrap().is_empty(),
        "取消的 operation 不得登记任何文件"
    );
}
