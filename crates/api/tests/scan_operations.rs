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

/// P2-2 端到端：同一 target 的第二个事件虽然不改变目标列表，但必须让该 target 被
/// 重新处理；终态报告是整轮累计结果（P3-3），不会因为多轮而清零。
#[tokio::test]
async fn repeated_watch_event_for_same_target_is_consumed_with_cumulative_report() {
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
    assert_eq!(done.dirty_rev, 1);
    // 本轮已带上合并后的意图：新文件入账，终态报告是累计结果而不是被清空。
    assert_eq!(
        done.report["scanned"], 1,
        "合并事件必须让 target 被重新处理: {}",
        done.report
    );
    assert_eq!(done.report["inserted"], 1, "{}", done.report);

    let rows = fix.state.store().lock().list_ledger().unwrap();
    let paths: Vec<&str> = rows.iter().map(|row| row.path.as_str()).collect();
    assert!(
        paths.iter().any(|p| p.ends_with("Movie2 (2024).mkv")),
        "补扫轮的新文件必须入账: {paths:?}"
    );
    assert_eq!(rows.len(), 2);
}

/// P3-3 端到端：chunk 前缀已提交、后续 chunk 失败时，终态报告必须保留已提交计数与
/// 错误摘要，而不是只剩 http_status。
#[tokio::test]
async fn committed_chunk_prefix_survives_a_later_chunk_failure() {
    let fix = PresenceFixture::new();
    // 给 Library A 增加一个与 Library B 同深度的根：sub 里的文件归属歧义，root 里的
    // 文件仍然唯一归属 A，于是第一个 chunk 提交、第二个 chunk 必然失败。
    let sub = fix.root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    for i in 0..101 {
        std::fs::write(fix.root.join(format!("Movie_{i:03} (2024).mkv")), b"x").unwrap();
    }
    std::fs::write(sub.join("Movie_sub (2024).mkv"), b"x").unwrap();
    {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .update_library(
                &fix.library_id,
                None,
                Some(&[fix.root.to_str().unwrap(), sub.to_str().unwrap()]),
                None,
            )
            .unwrap();
        guard
            .create_library(
                MediaKind::Movie,
                "Nested",
                &[sub.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
    }
    let sig = fix.scope_signature();
    let op = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .enqueue_scan(&fix.library_id, &sig, &[], 1000)
            .unwrap()
    };

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_scan_terminal_op(&op.id).await;
    assert_eq!(
        done.status,
        store::ScanOperationStatus::Failed,
        "{:?}",
        done.report
    );
    assert_eq!(
        done.report["scanned"], 100,
        "失败终态必须保留已提交前缀: {}",
        done.report
    );
    assert_eq!(done.report["inserted"], 100, "{}", done.report);
    assert!(
        done.report["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()),
        "失败终态必须写错误摘要: {}",
        done.report
    );
    let rows = fix.state.store().lock().list_ledger().unwrap();
    assert_eq!(rows.len(), 100, "只有已提交的 chunk 前缀入账");
}

/// P2-NEW-1：上一个构建写下的 v1 签名，配置未变时不得被当成 scope 变化取消；
/// 必须就地把持久化签名升级成新编码，并照常完成扫描。
#[tokio::test]
async fn legacy_signature_with_unchanged_config_is_upgraded_not_cancelled() {
    let fix = PresenceFixture::new();
    let file = fix.root.join("Movie1 (2024).mkv");
    std::fs::write(&file, b"1").unwrap();
    let legacy = fix.legacy_scope_signature();
    let current = fix.scope_signature();
    assert_ne!(legacy, current, "测试前提：旧编码与新编码不同");
    // 单根、路径不含逗号：v1 编码可唯一还原，逐字节相等就是配置相等的证明（P2-A）。
    assert_eq!(
        legacy.split(',').count(),
        1,
        "测试前提：这是可证明等价的非碰撞旧编码: {legacy}"
    );

    let op = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .enqueue_scan(&fix.library_id, &legacy, &[], 1000)
            .unwrap()
    };

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_scan_terminal_op(&op.id).await;
    assert_eq!(
        done.status,
        store::ScanOperationStatus::Completed,
        "配置未变时旧签名 operation 不得消失: {}",
        done.report
    );
    assert_eq!(
        done.scope_signature, current,
        "旧签名必须就地升级成新编码"
    );
    assert!(
        done.report["signature_upgrade"].is_string(),
        "升级必须留下显式原因: {}",
        done.report
    );
    assert_eq!(fix.state.store().lock().list_ledger().unwrap().len(), 1);
}

/// P2-A：v1 编码不是单射——两组不同的根数组可以编码出同一串字节，逐字节相等不能
/// 证明配置相等。带着旧根数组签名的 operation 绝不能被升级成当前 scope 去扫新根。
///
/// 碰撞与任务给出的例子同形，只是把根放在 tempdir 下以便新 scope 里真的有文件：
///   旧根 `[<P>/a, b,<P>/c]` 与当前根 `[<P>/a,b, <P>/c]`
///   都编码成 `movie:2:<P>/a,b,<P>/c`。
#[tokio::test]
async fn colliding_legacy_signature_never_adopts_current_scope() {
    let fix = PresenceFixture::new();
    let base = fix.dir.path().to_string_lossy().to_string();
    // 当前（新）配置：第二个根路径自身含逗号，v1 的逗号拼接无法唯一还原。
    let comma_root = format!("{base}/a,b");
    let plain_root = format!("{base}/c");
    std::fs::create_dir_all(&comma_root).unwrap();
    std::fs::create_dir_all(&plain_root).unwrap();
    // 新 scope 里放真实文件：修复前会被登记进台账，这正是必须为 0 的行。
    std::fs::write(format!("{comma_root}/Movie Comma (2024).mkv"), b"1").unwrap();
    std::fs::write(format!("{plain_root}/Movie Plain (2024).mkv"), b"2").unwrap();
    {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .update_library(
                &fix.library_id,
                None,
                Some(&[comma_root.as_str(), plain_root.as_str()]),
                None,
            )
            .unwrap();
    }

    // 旧配置的根数组：只留在遗留 operation 的签名里，配置里已经不存在。
    let old_roots = [format!("{base}/a"), format!("b,{base}/c")];
    let legacy = format!("movie:{}:{}", old_roots.len(), old_roots.join(","));
    assert_eq!(
        legacy,
        fix.legacy_scope_signature(),
        "测试前提：旧根数组与当前根数组的 v1 编码逐字节碰撞"
    );
    assert_ne!(legacy, fix.scope_signature(), "测试前提：遗留签名仍是旧编码");

    let op = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .enqueue_scan(&fix.library_id, &legacy, &[], 1000)
            .unwrap()
    };

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_scan_terminal_op(&op.id).await;
    assert_eq!(
        done.status,
        store::ScanOperationStatus::Cancelled,
        "碰撞的遗留签名无法证明等价，必须取消而不是扫当前 scope: {}",
        done.report
    );
    assert_eq!(done.report["error"], "library.scope_changed");
    assert!(
        done.report["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("not provably equivalent")),
        "取消原因必须说明旧编码无法证明等价: {}",
        done.report
    );
    assert_eq!(
        done.scope_signature, legacy,
        "取消时不得把持久化签名改写成当前 scope"
    );
    assert!(
        fix.state.store().lock().list_ledger().unwrap().is_empty(),
        "取消的 operation 不得把当前 scope 的文件登记进台账"
    );
}

/// P2-A：旧编码有歧义（根路径含逗号）时，即使**逐字节等于**当前配置重新编码出的旧格式，
/// 也无法证明这行来自同一配置——必须取消，绝不升级成当前 scope。
#[tokio::test]
async fn ambiguous_legacy_signature_equal_to_current_encoding_is_cancelled() {
    let fix = PresenceFixture::new();
    // 第二个根路径带逗号：v1 的逗号拼接无法唯一还原（`split(',')` 段数多于 count）。
    let comma_root = fix.dir.path().join("movies,alt");
    std::fs::create_dir_all(&comma_root).unwrap();
    {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .update_library(
                &fix.library_id,
                None,
                Some(&[fix.root.to_str().unwrap(), comma_root.to_str().unwrap()]),
                None,
            )
            .unwrap();
    }
    std::fs::write(fix.root.join("Movie1 (2024).mkv"), b"1").unwrap();
    std::fs::write(comma_root.join("Movie2 (2024).mkv"), b"2").unwrap();

    let legacy = fix.legacy_scope_signature();
    let current = fix.scope_signature();
    assert_ne!(legacy, current, "测试前提：旧编码与新编码不同");
    assert!(
        legacy.split(',').count() > 2,
        "测试前提：旧编码按逗号切分多于 count，无法唯一还原: {legacy}"
    );
    let op = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .enqueue_scan(&fix.library_id, &legacy, &[], 1000)
            .unwrap()
    };

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_scan_terminal_op(&op.id).await;
    assert_eq!(
        done.status,
        store::ScanOperationStatus::Cancelled,
        "无法唯一还原的旧编码必须取消，不能升级成当前 scope: {}",
        done.report
    );
    assert_eq!(done.report["error"], "library.scope_changed");
    assert!(
        done.report["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("not provably equivalent")),
        "取消原因必须说明旧编码无法证明等价: {}",
        done.report
    );
    assert_eq!(done.scope_signature, legacy, "取消时不得改写签名");
    assert!(
        fix.state.store().lock().list_ledger().unwrap().is_empty(),
        "取消的 operation 不得登记任何文件"
    );
}

/// P2-NEW-1（安全）：旧编码有歧义、且与当前配置重新编码出的旧格式**不同**时，无法证明
/// 等价，必须取消，绝不能升级成当前配置去扫新 scope。
///
/// 场景：旧根 `/old,movies`（签名 `movie:1:/old,movies`），根随后改成 `/new`；
/// 逗号切分 2 段 ≠ count 1。修复前会被判为「只是格式歧义」并升级成 `/new` 的签名，
/// 于是照扫新 scope 并把 `/new` 的文件登记进台账。
#[tokio::test]
async fn ambiguous_legacy_signature_after_scope_change_is_cancelled() {
    let fix = PresenceFixture::new();
    let old_root = fix.dir.path().join("old,movies");
    let new_root = fix.dir.path().join("new");
    std::fs::create_dir_all(&old_root).unwrap();
    std::fs::create_dir_all(&new_root).unwrap();
    std::fs::write(old_root.join("Old (2024).mkv"), b"old").unwrap();
    std::fs::write(new_root.join("New (2024).mkv"), b"new").unwrap();

    // 1. 旧配置：唯一根路径里含逗号 → 旧编码 `movie:1:<old_root>` 有歧义。
    {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .update_library(
                &fix.library_id,
                None,
                Some(&[old_root.to_str().unwrap()]),
                None,
            )
            .unwrap();
    }
    let legacy = fix.legacy_scope_signature();
    assert!(
        legacy.contains("old,movies"),
        "测试前提：旧签名来自含逗号的旧根: {legacy}"
    );
    assert_eq!(
        legacy.split(',').count(),
        2,
        "测试前提：旧编码逗号切分 2 段 ≠ count 1: {legacy}"
    );

    // 2. operation 入队前配置已经改成另一个根。
    {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .update_library(
                &fix.library_id,
                None,
                Some(&[new_root.to_str().unwrap()]),
                None,
            )
            .unwrap();
    }

    // 3. 遗留 operation 仍带着旧签名。
    let op = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .enqueue_scan(&fix.library_id, &legacy, &[], 1000)
            .unwrap()
    };

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_scan_terminal_op(&op.id).await;
    assert_eq!(
        done.status,
        store::ScanOperationStatus::Cancelled,
        "无法证明等价的旧签名必须取消，而不是升级到当前配置: {}",
        done.report
    );
    assert_eq!(done.report["error"], "library.scope_changed");
    assert!(
        done.report["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("not provably equivalent")),
        "取消原因必须说明旧编码无法证明等价: {}",
        done.report
    );
    assert_eq!(done.scope_signature, legacy, "取消时不得改写签名");
    assert!(
        fix.state.store().lock().list_ledger().unwrap().is_empty(),
        "取消的 operation 不得登记任何文件（尤其不能扫新根）"
    );
}
