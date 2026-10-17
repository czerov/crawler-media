use std::path::PathBuf;

use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use store::{ScanOperationStatus, Store};

fn insert_ledger_row(store: &Store, path: &std::path::Path) {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Movie".to_string(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: media.id,
            path: path.display().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();
}

#[test]
fn enqueue_scan_deduplicates_active_operations() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let op1 = store
        .enqueue_scan("lib1", "sig1", &[], 1000)
        .unwrap();
    assert_eq!(op1.status, ScanOperationStatus::Queued);
    assert_eq!(op1.generation, 1);

    // Enqueueing again returns the existing operation
    let op2 = store
        .enqueue_scan("lib1", "sig1", &[], 1050)
        .unwrap();
    assert_eq!(op1.id, op2.id);

    // Cancel operation
    assert!(store.cancel_scan(&op1.id, 1100).unwrap());
    let op_cancelled = store.scan_operation(&op1.id).unwrap().unwrap();
    assert_eq!(op_cancelled.status, ScanOperationStatus::Cancelled);

    // Enqueueing after cancel creates generation 2
    let op3 = store
        .enqueue_scan("lib1", "sig1", &[], 1200)
        .unwrap();
    assert_ne!(op1.id, op3.id);
    assert_eq!(op3.generation, 2);
}

/// 增量扫描目标必须落库：单飞合并后的目标集在崩溃重排/重启后仍然生效。
#[test]
fn enqueue_scan_persists_and_merges_targets() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let show_a = PathBuf::from("/media/tv/Show A");
    let show_b = PathBuf::from("/media/tv/Show B");

    let op1 = store
        .enqueue_scan("lib1", "sig1", &[show_a.clone()], 1000)
        .unwrap();
    assert_eq!(op1.targets, vec![show_a.clone()]);

    // 单飞：新目标并进同一个 operation，且写进行里。
    let op2 = store
        .enqueue_scan("lib1", "sig1", &[show_a.clone(), show_b.clone()], 1050)
        .unwrap();
    assert_eq!(op1.id, op2.id);
    assert_eq!(op2.targets, vec![show_a.clone(), show_b.clone()]);
    let reloaded = store.scan_operation(&op1.id).unwrap().unwrap();
    assert_eq!(
        reloaded.targets,
        vec![show_a.clone(), show_b.clone()],
        "合并后的目标集必须持久化"
    );

    // 认领时也要带上目标集，worker 才能只扫这些子目录。
    let claimed = store.claim_next_queued_scan(&[]).unwrap().unwrap();
    assert_eq!(claimed.targets, vec![show_a, show_b]);
    let listed = store.scan_operations_for_library("lib1").unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].targets.len(), 2);
}

/// P2-1 方向一：排队中的整库扫描被 watch 目标合并后，语义必须仍然是整库。
#[test]
fn full_scan_intent_is_not_narrowed_by_target_merge() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let show_a = PathBuf::from("/media/tv/Show A");

    let op1 = store.enqueue_scan("lib1", "sig1", &[], 1000).unwrap();
    assert!(op1.is_full_scope());
    assert!(op1.targets.is_empty());

    let op2 = store
        .enqueue_scan("lib1", "sig1", &[show_a.clone()], 1050)
        .unwrap();
    assert_eq!(op1.id, op2.id, "单飞：仍应是同一个 operation");
    assert!(op2.is_full_scope(), "整库意图不得被增量目标缩小");
    assert!(op2.targets.is_empty(), "full 语义下目标列表没有意义");

    // 认领时也要带 full 语义，runner 才会走库根而不是目标子目录。
    let claimed = store.claim_next_queued_scan(&[]).unwrap().unwrap();
    assert!(claimed.is_full_scope());
    assert!(claimed.targets.is_empty());
}

/// P2-1 方向二：排队的增量扫描遇到管理员整库请求，必须升级为整库而不是被忽略。
#[test]
fn admin_full_request_upgrades_targeted_operation() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let show_a = PathBuf::from("/media/tv/Show A");

    let op1 = store
        .enqueue_scan("lib1", "sig1", &[show_a.clone()], 1000)
        .unwrap();
    assert!(!op1.is_full_scope());
    assert_eq!(op1.scope_kind_str(), "targeted");

    let op2 = store.enqueue_scan("lib1", "sig1", &[], 1050).unwrap();
    assert_eq!(op1.id, op2.id);
    assert!(op2.is_full_scope(), "整库请求必须把增量 operation 升级为整库");
    assert!(op2.targets.is_empty());

    let reloaded = store.scan_operation(&op1.id).unwrap().unwrap();
    assert!(reloaded.is_full_scope());
    assert_eq!(reloaded.scope_kind_str(), "full");
}

/// P2-2：同一 target 的重复事件不能让目标列表「看起来没变」就被吞掉。
#[test]
fn repeated_target_enqueue_bumps_dirty_rev() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let show_a = PathBuf::from("/media/tv/Show A");
    let show_b = PathBuf::from("/media/tv/Show B");

    let op1 = store
        .enqueue_scan("lib1", "sig1", &[show_a.clone()], 1000)
        .unwrap();
    assert_eq!(op1.dirty_rev, 0, "新建 operation 尚未执行，无需补扫版本");
    assert_eq!(op1.consumed_rev, 0);

    // 同一 target 再来一次：列表不变，但 dirty_rev 必须前进。
    let op2 = store
        .enqueue_scan("lib1", "sig1", &[show_a.clone()], 1001)
        .unwrap();
    assert_eq!(op1.id, op2.id);
    assert_eq!(op2.targets, vec![show_a.clone()]);
    assert_eq!(op2.dirty_rev, 1);
    assert_eq!(op2.consumed_rev, 0);

    let op3 = store
        .enqueue_scan("lib1", "sig1", &[show_b.clone()], 1002)
        .unwrap();
    assert_eq!(op3.targets, vec![show_a, show_b]);
    assert_eq!(op3.dirty_rev, 2);

    // 消费后不再需要补扫；版本单调不回退。
    assert!(store.advance_scan_consumed_rev(&op1.id, 2).unwrap());
    let reloaded = store.scan_operation(&op1.id).unwrap().unwrap();
    assert_eq!(reloaded.consumed_rev, 2);
    assert_eq!(reloaded.dirty_rev, 2);
    assert!(!store.advance_scan_consumed_rev(&op1.id, 1).unwrap());
    assert_eq!(
        store.scan_operation(&op1.id).unwrap().unwrap().consumed_rev,
        2
    );
}

/// P2-3：断点命中必须复核台账存在性、missing 标记与文件版本。
#[test]
fn scan_seen_requires_current_ledger_and_file_version() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("media");
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("Movie (2024).mkv");
    std::fs::write(&file, b"one").unwrap();
    let path = file.display().to_string();

    let op = store.enqueue_scan("lib1", "sig1", &[], 1000).unwrap();
    assert!(!store.scan_seen_is_current(&op.id, &path).unwrap(), "未记录断点");

    // 文件存在但没有台账行：断点无效，必须重新入账。
    store.record_scan_seen(&op.id, &[file.clone()]).unwrap();
    assert!(
        !store.scan_seen_is_current(&op.id, &path).unwrap(),
        "台账行不存在时必须重新处理"
    );

    // 补上台账行且未 missing：版本一致时可以跳过。
    insert_ledger_row(&store, &file);
    assert!(store.scan_seen_is_current(&op.id, &path).unwrap());

    // 内容变化（size 变）：必须重新处理。
    std::fs::write(&file, b"one-changed-content").unwrap();
    assert!(
        !store.scan_seen_is_current(&op.id, &path).unwrap(),
        "文件版本变化时必须重新处理"
    );

    // 恢复场景：文件被删除并标记 missing，再恢复后即使版本相同也必须重新处理。
    store.record_scan_seen(&op.id, &[file.clone()]).unwrap();
    std::fs::remove_file(&file).unwrap();
    let (missing, _restored) = store.verify_library_files(std::slice::from_ref(&root), 1).unwrap();
    assert_eq!(missing, 1, "文件不存在时应标记 missing");
    std::fs::write(&file, b"one-changed-content").unwrap();
    assert!(
        !store.scan_seen_is_current(&op.id, &path).unwrap(),
        "恢复的文件必须清掉 missing 并重新处理"
    );
}

/// P2-3：STRM 只有 URL 文本，同 size/同 mtime 的替换必须靠内容指纹识别。
#[test]
fn strm_url_change_with_same_size_and_mtime_is_not_current() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("media");
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("Movie (2024).strm");
    std::fs::write(&file, "https://a/video").unwrap();
    let path = file.display().to_string();

    let op = store.enqueue_scan("lib1", "sig1", &[], 1000).unwrap();
    insert_ledger_row(&store, &file);
    store.record_scan_seen(&op.id, &[file.clone()]).unwrap();
    assert!(
        store.scan_seen_is_current(&op.id, &path).unwrap(),
        "URL 未变化时断点应命中"
    );

    let original_mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
    // 同长度 URL 替换，并把 mtime 恢复成原值：size + mtime 完全一致。
    std::fs::write(&file, "https://b/video").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(original_mtime)
        .unwrap();
    assert!(
        !store.scan_seen_is_current(&op.id, &path).unwrap(),
        "同 size/同 mtime 的 STRM URL 变化必须视为变化"
    );
}

/// P2-3：老断点行的 0/0 是「版本未知」，绝不视为 current。
#[test]
fn unknown_zero_version_checkpoint_is_not_current() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("media");
    let missing_file = root.join("Gone (2024).mkv");
    let path = missing_file.display().to_string();

    let op = store.enqueue_scan("lib1", "sig1", &[], 1000).unwrap();
    // 台账行存在，但文件读不到 metadata → 断点版本 0/0（等价于老断点行）。
    insert_ledger_row(&store, &missing_file);
    store
        .record_scan_seen(&op.id, &[missing_file.clone()])
        .unwrap();
    assert!(
        !store.scan_seen_is_current(&op.id, &path).unwrap(),
        "版本未知的断点行绝不视为 current"
    );
}

/// P3-1：终态判定必须与写入同一事务；有新事件时补扫而不是覆盖。
#[test]
fn finalize_scan_refuses_to_overwrite_new_dirty_event() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let show_a = PathBuf::from("/media/tv/Show A");
    let op = store
        .enqueue_scan("lib1", "sig1", &[show_a.clone()], 1000)
        .unwrap();
    let claimed = store.claim_next_queued_scan(&[]).unwrap().unwrap();
    assert_eq!(claimed.id, op.id);
    assert_eq!(claimed.dirty_rev, 0, "本轮消费的版本");

    // worker 判定之后、写终态之前，另一请求并入同一 operation。
    let merged = store
        .enqueue_scan("lib1", "sig1", &[show_a.clone()], 1001)
        .unwrap();
    assert_eq!(merged.id, op.id);
    assert_eq!(merged.dirty_rev, 1);

    let report = serde_json::json!({ "scanned": 3, "inserted": 2 });
    let outcome = store
        .finalize_or_requeue_scan(&op.id, 0, ScanOperationStatus::Completed, &report)
        .unwrap();
    assert_eq!(outcome, Some(1), "有新事件时必须补扫而不是写终态");
    let requeued = store.scan_operation(&op.id).unwrap().unwrap();
    assert_eq!(requeued.status, ScanOperationStatus::Queued);
    assert_eq!(requeued.consumed_rev, 1);
    assert_eq!(requeued.report, report, "累计报告必须保留到下一轮");

    // 补扫轮消费到当前版本后，才允许写终态并清理断点。
    let claimed = store.claim_next_queued_scan(&[]).unwrap().unwrap();
    assert_eq!(claimed.dirty_rev, 1);
    let seen_file = tmp.path().join("seen.mkv");
    std::fs::write(&seen_file, b"x").unwrap();
    store.record_scan_seen(&op.id, &[seen_file.clone()]).unwrap();
    assert!(store
        .is_scan_seen(&op.id, &seen_file.display().to_string())
        .unwrap());
    assert_eq!(
        store
            .finalize_or_requeue_scan(&op.id, 1, ScanOperationStatus::Completed, &report)
            .unwrap(),
        None
    );
    let done = store.scan_operation(&op.id).unwrap().unwrap();
    assert_eq!(done.status, ScanOperationStatus::Completed);
    assert_eq!(done.report, report);
    assert!(
        !store
            .is_scan_seen(&op.id, &seen_file.display().to_string())
            .unwrap(),
        "写终态必须同时清理断点"
    );
}


/// P3-NEW-1：补扫轮标记 Running 与崩溃恢复都不得清空 requeue 落库的累计报告。
#[test]
fn crash_recovery_keeps_requeued_round_report() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("media");
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let op = store.enqueue_scan(&library.id, "sig1", &[], 1000).unwrap();
    assert!(store.claim_next_queued_scan(&[]).unwrap().is_some());

    // 轮内并入事件 → 补扫重排，累计报告落库。
    store.enqueue_scan(&library.id, "sig1", &[], 1001).unwrap();
    let report = serde_json::json!({
        "queued": true,
        "transferred": true,
        "status": "completed",
        "scanned": 7,
        "inserted": 3,
        "restored": 0,
        "probe_pending": 3,
        "roots": [],
        "error_count": 0,
        "errors_truncated": false,
        "issues": [],
    });
    assert_eq!(
        store
            .finalize_or_requeue_scan(&op.id, 0, ScanOperationStatus::Completed, &report)
            .unwrap(),
        Some(1)
    );

    // 第二轮认领（Running）→ 只改状态，不得覆盖报告。
    assert!(store.claim_next_queued_scan(&[]).unwrap().is_some());
    assert!(store
        .set_scan_operation_status(&op.id, ScanOperationStatus::Running)
        .unwrap());
    assert_eq!(
        store.scan_operation(&op.id).unwrap().unwrap().report,
        report,
        "Running 转换不得清空累计报告"
    );

    // 模拟崩溃恢复：running → queued，报告仍在。
    assert_eq!(store.recover_orphaned_scans().unwrap(), 1);
    let recovered = store.scan_operation(&op.id).unwrap().unwrap();
    assert_eq!(recovered.status, ScanOperationStatus::Queued);
    assert_eq!(recovered.report, report, "崩溃恢复后累计报告必须保留");
}

/// P2-3：durable 源版本挂在台账行上，跨 operation 存活（`scan_seen` 被清空后仍在），
/// 并在该路径被重新处理时前进。
#[test]
fn ledger_source_version_survives_operation_checkpoint_clear() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("media");
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("Movie (2024).strm");
    std::fs::write(&file, "https://a/video").unwrap();
    let path = file.display().to_string();
    insert_ledger_row(&store, &file);

    // 首次处理：断点与 durable 版本一起写下。
    let op1 = store.enqueue_scan("lib1", "sig1", &[], 1000).unwrap();
    store.record_scan_seen(&op1.id, &[file.clone()]).unwrap();
    let recorded = store
        .ledger_source_versions(&[path.clone()])
        .unwrap()
        .remove(&path)
        .expect("已处理的路径必须有 durable 源版本");
    assert!(
        recorded.2.as_deref().is_some_and(|digest| !digest.is_empty()),
        "STRM 必须有受限内容指纹"
    );

    // 终态清空断点：durable 版本必须还在，且能证明未变化的文件是同一版本。
    assert_eq!(
        store
            .finalize_or_requeue_scan(&op1.id, 0, ScanOperationStatus::Completed, &serde_json::json!({}))
            .unwrap(),
        None
    );
    assert!(!store.is_scan_seen(&op1.id, &path).unwrap());
    let surviving = store.ledger_source_versions(&[path.clone()]).unwrap();
    assert_eq!(surviving.get(&path), Some(&recorded), "断点清空不得丢掉 durable 版本");
    assert!(Store::scan_seen_stamp_matches(
        &recorded,
        &Store::scan_seen_stamp(&file)
    ));

    // 同 size/同 mtime 的 URL 替换：版本不再匹配，重新处理后前进。
    let original_mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
    std::fs::write(&file, "https://b/video").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(original_mtime)
        .unwrap();
    assert!(
        !Store::scan_seen_stamp_matches(&recorded, &Store::scan_seen_stamp(&file)),
        "同 size/同 mtime 的 URL 变化必须被 durable 版本识别"
    );

    let op2 = store.enqueue_scan("lib1", "sig1", &[], 2000).unwrap();
    assert_ne!(op1.id, op2.id, "上一个 operation 已终态");
    store.record_scan_seen(&op2.id, &[file.clone()]).unwrap();
    let advanced = store
        .ledger_source_versions(&[path.clone()])
        .unwrap()
        .remove(&path)
        .expect("重新处理必须刷新 durable 源版本");
    assert_ne!(advanced, recorded, "重新处理后 durable 版本必须前进");
    assert!(Store::scan_seen_stamp_matches(
        &advanced,
        &Store::scan_seen_stamp(&file)
    ));

    // 没有台账行的路径不返回版本（本轮新文件，没有派生事实需要失效）。
    let fresh = root.join("New (2024).mkv");
    std::fs::write(&fresh, b"x").unwrap();
    assert!(
        store
            .ledger_source_versions(&[fresh.display().to_string()])
            .unwrap()
            .is_empty()
    );
}

/// P2-NEW-1：签名升级是 CAS，旧值不匹配时不写入、也不会覆盖别人的值。
#[test]
fn scope_signature_upgrade_is_compare_and_swap() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let op = store
        .enqueue_scan("lib1", "movie:1:/old", &[], 1000)
        .unwrap();

    assert!(
        !store
            .upgrade_scan_scope_signature(&op.id, "movie:1:/other", "v2:movie:1:4:/old")
            .unwrap(),
        "期望值不匹配时不得写入"
    );
    assert!(store
        .upgrade_scan_scope_signature(&op.id, "movie:1:/old", "v2:movie:1:4:/old")
        .unwrap());
    assert_eq!(
        store.scan_operation(&op.id).unwrap().unwrap().scope_signature,
        "v2:movie:1:4:/old"
    );
    assert!(
        !store
            .upgrade_scan_scope_signature(&op.id, "movie:1:/old", "v2:movie:1:4:/other")
            .unwrap(),
        "已升级的签名不得被重复升级覆盖"
    );
}
