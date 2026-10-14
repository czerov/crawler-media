#[path = "support/mod.rs"]
mod support;

use axum::http::StatusCode;
use domain::MediaKind;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn scan_returns_202_with_operation_id_and_polls() {
    let fix = PresenceFixture::new();

    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/scan", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(res["ok"], true);
    assert_eq!(res["data"]["queued"], true);

    let op_id = res["data"]["operation_id"].as_str().expect("must return operation_id");

    // Poll operation status
    let (poll_status, poll_res) = fix
        .request(
            "GET",
            &format!("/api/v1/libraries/{}/scan-operations/{}", fix.library_id, op_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(poll_status, StatusCode::OK);
    assert_eq!(poll_res["ok"], true);
    assert_eq!(poll_res["data"]["operation_id"], op_id);
}

#[tokio::test]
async fn cancel_scan_does_not_break_subsequent_library_work_leases() {
    let fix = PresenceFixture::new();

    // 1. Enqueue scan
    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/scan", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let op_id = res["data"]["operation_id"].as_str().unwrap();

    // 2. Cancel scan with wrong library id -> 404
    let (wrong_status, _) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/wrong-lib-id/scan-operations/{}/cancel", op_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(wrong_status, StatusCode::NOT_FOUND);

    // 3. Cancel scan with correct library id -> 200
    let (cancel_status, cancel_res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/scan-operations/{}/cancel", fix.library_id, op_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(cancel_status, StatusCode::OK);
    assert_eq!(cancel_res["ok"], true);

    // 3b. 取消是幂等的：同一个操作再取消一次仍须 200（cancelled=false），不是 404。
    let (again_status, again_res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/scan-operations/{}/cancel", fix.library_id, op_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(again_status, StatusCode::OK, "重复取消必须幂等: {again_res}");
    assert_eq!(again_res["data"]["cancelled"], false);

    // 4. Critical: library work lease must NOT be permanently broken!
    let lease_res = fix.state.acquire_library_work(&fix.library_id);
    assert!(lease_res.is_ok(), "Canceling a scan must not freeze the library permanently");

    // 5. Subsequent scan must be accepted and **actually complete** — a
    //    cancelled shared control would leave it failed/partial.
    let next_res = fix.scan_and_wait_completed(&fix.library_id).await;
    assert_eq!(next_res["status"], "completed");
}

/// P2-1 端到端：先排整库、再把 watch 目标 A 合并进来，整库语义不得被缩小，
/// A 与 B 都必须登记。
#[tokio::test]
async fn full_scan_merged_with_watch_target_still_ledgers_whole_library() {
    let fix = PresenceFixture::new();
    let a_dir = fix.root.join("Show A");
    let b_dir = fix.root.join("Show B");
    std::fs::create_dir_all(&a_dir).unwrap();
    std::fs::create_dir_all(&b_dir).unwrap();
    std::fs::write(a_dir.join("Movie A (2024).mkv"), b"a").unwrap();
    std::fs::write(b_dir.join("Movie B (2024).mkv"), b"b").unwrap();
    let sig = fix.scope_signature();

    let op = {
        let store = fix.state.store();
        let guard = store.lock();
        let full = guard
            .enqueue_scan(&fix.library_id, &sig, &[], 1000)
            .unwrap();
        assert!(full.is_full_scope());
        let merged = guard
            .enqueue_scan(&fix.library_id, &sig, &[a_dir.clone()], 1001)
            .unwrap();
        assert_eq!(full.id, merged.id, "单飞：仍是同一个 operation");
        assert!(merged.is_full_scope(), "整库意图不得被目标合并缩小");
        merged
    };

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_scan_terminal_op(&op.id).await;
    assert_eq!(done.status, store::ScanOperationStatus::Completed, "{:?}", done.report);

    let rows = fix.state.store().lock().list_ledger().unwrap();
    let paths: Vec<&str> = rows.iter().map(|row| row.path.as_str()).collect();
    assert!(
        paths.iter().any(|p| p.ends_with("Movie A (2024).mkv")),
        "目标 A 必须入账: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("Movie B (2024).mkv")),
        "整库扫描必须登记 B（不得被缩成 A）: {paths:?}"
    );
    assert_eq!(rows.len(), 2);
}

/// P2-2 端到端：同一 target 的第二个事件虽然不改变目标列表，但必须驱动补扫一轮；
/// 断点命中的旧文件不重复处理，新文件最终入账。
#[tokio::test]
async fn repeated_watch_event_for_same_target_runs_followup_round() {
    let fix = PresenceFixture::new();
    let dir = fix.root.join("Show A");
    std::fs::create_dir_all(&dir).unwrap();
    let f1 = dir.join("Movie1 (2024).mkv");
    std::fs::write(&f1, b"1").unwrap();
    // f1 已由上一轮登记过：预置台账 + 断点，模拟「A 正在扫时 A 又变了」。
    fix.insert_media_and_ledger("Show A/Movie1 (2024).mkv");
    let sig = fix.scope_signature();

    let op = {
        let store = fix.state.store();
        let guard = store.lock();
        let op = guard
            .enqueue_scan(&fix.library_id, &sig, &[dir.clone()], 1000)
            .unwrap();
        guard.record_scan_seen(&op.id, &[f1.clone()]).unwrap();
        let merged = guard
            .enqueue_scan(&fix.library_id, &sig, &[dir.clone()], 1001)
            .unwrap();
        assert_eq!(op.id, merged.id);
        assert_eq!(merged.dirty_rev, 1, "重复事件必须推进 dirty_rev");
        op
    };

    let f2 = dir.join("Movie2 (2024).mkv");
    std::fs::write(&f2, b"2").unwrap();

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_scan_terminal_op(&op.id).await;
    assert_eq!(done.status, store::ScanOperationStatus::Completed, "{:?}", done.report);
    assert_eq!(done.consumed_rev, 1, "补扫轮必须消费 dirty 版本");
    assert_eq!(done.dirty_rev, 1);
    // 终态报告来自补扫轮：两个文件都命中断点被跳过，说明确实多跑了一轮。
    assert_eq!(
        done.report["scanned"], 0,
        "终态报告应来自补扫轮（第一轮已处理新文件）: {}",
        done.report
    );

    let rows = fix.state.store().lock().list_ledger().unwrap();
    let paths: Vec<&str> = rows.iter().map(|row| row.path.as_str()).collect();
    assert!(
        paths.iter().any(|p| p.ends_with("Movie2 (2024).mkv")),
        "补扫轮的新文件必须入账: {paths:?}"
    );
    assert_eq!(rows.len(), 2);
}
