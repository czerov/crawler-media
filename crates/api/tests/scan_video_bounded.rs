//! P3-2：Video（「其他」）库扫描必须走与 Movie/TV 同一套有界执行器：
//! 定向扫描只走目标目录、断点续扫不重复处理、变化文件重新处理。

#[path = "support/mod.rs"]
mod support;

use std::path::{Path, PathBuf};

use domain::MediaKind;
use store::{Library, ScanOperation};
use support::library_presence::PresenceFixture;

/// 建一个 Video 库并返回它；调用方自行准备根目录内容。
fn create_video_library(fix: &PresenceFixture, root: &Path) -> Library {
    let store = fix.state.store();
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
}

fn signature(fix: &PresenceFixture, library_id: &str) -> String {
    let store = fix.state.store();
    let guard = store.lock();
    let library = guard.get_library(library_id).unwrap().expect("library");
    api::scan_operations::scan_scope_signature(&library)
}

fn enqueue(
    fix: &PresenceFixture,
    library_id: &str,
    sig: &str,
    targets: &[PathBuf],
) -> ScanOperation {
    let store = fix.state.store();
    let guard = store.lock();
    guard
        .enqueue_scan(library_id, sig, targets, 1000)
        .unwrap()
}

/// 预置一条派生探测事实：变化文件的失效必须能被观察，而不是靠探测队列的时序。
fn seed_file_meta(fix: &PresenceFixture, ledger_id: &str) {
    let tracks = library::Tracks {
        video: Some(library::VideoTrack {
            codec: Some("h264".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let store = fix.state.store();
    let guard = store.lock();
    guard
        .put_file_meta_versioned(ledger_id, &tracks, Some("v1"), Some(1000))
        .unwrap();
    assert!(guard.get_file_meta(ledger_id).unwrap().is_some(), "前置事实应存在");
}

/// P3-2(a)：定向 Video operation 只遍历目标目录，绝不能被静默降级成整库扫描。
#[tokio::test]
async fn targeted_video_scan_walks_only_its_targets() {
    let fix = PresenceFixture::new();
    let video_root = fix.dir.path().join("video_targeted");
    let target_dir = video_root.join("Trip");
    let other_dir = video_root.join("Wedding");
    std::fs::create_dir_all(&target_dir).unwrap();
    std::fs::create_dir_all(&other_dir).unwrap();
    let library = create_video_library(&fix, &video_root);
    let target_file = target_dir.join("clip-target.mp4");
    let other_file = other_dir.join("clip-other.mp4");
    std::fs::write(&target_file, b"target").unwrap();
    std::fs::write(&other_file, b"other").unwrap();

    let sig = signature(&fix, &library.id);
    let op = enqueue(&fix, &library.id, &sig, &[target_dir.clone()]);
    assert!(!op.is_full_scope(), "定向 operation 不得被当成整库");
    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_scan_terminal_op(&op.id).await;
    assert_eq!(
        done.status,
        store::ScanOperationStatus::Completed,
        "{:?}",
        done.report
    );

    let store = fix.state.store();
    let guard = store.lock();
    assert!(
        guard
            .ledger_by_path(&target_file.display().to_string())
            .unwrap()
            .is_some(),
        "目标目录里的文件必须入账"
    );
    assert!(
        guard
            .ledger_by_path(&other_file.display().to_string())
            .unwrap()
            .is_none(),
        "定向 Video 扫描不得触碰无关目录: {:?}",
        done.report
    );
    drop(guard);
    assert_eq!(
        done.report["scanned"], 1,
        "被遍历的文件集合必须恰好是目标: {}",
        done.report
    );
}

/// P3-2(b)：断点续扫不重复处理；内容变化的文件必须重新处理并失效派生事实。
#[tokio::test]
async fn video_scan_checkpoint_skips_seen_and_reprocesses_changed() {
    let fix = PresenceFixture::new();
    let video_root = fix.dir.path().join("video_resume");
    std::fs::create_dir_all(&video_root).unwrap();
    let library = create_video_library(&fix, &video_root);
    let clip = video_root.join("holiday.mp4");
    std::fs::write(&clip, b"v1").unwrap();
    let sig = signature(&fix, &library.id);

    // 第一轮：真实扫完并记下台账 + 派生事实。
    let first_op = enqueue(&fix, &library.id, &sig, &[]);
    fix.state.scan_coordinator.trigger_next(&fix.state);
    let first = fix.wait_scan_terminal_op(&first_op.id).await;
    assert_eq!(first.status, store::ScanOperationStatus::Completed, "{:?}", first.report);
    assert_eq!(first.report["scanned"], 1, "{}", first.report);
    let ledger_id = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .ledger_by_path(&clip.display().to_string())
            .unwrap()
            .expect("clip 应已入账")
            .id
            .to_string()
    };
    seed_file_meta(&fix, &ledger_id);

    // 第二轮：预置断点（模拟上一轮已处理、进程重启），未变化的文件不得再处理。
    let resume_op = enqueue(&fix, &library.id, &sig, &[]);
    {
        let store = fix.state.store();
        let guard = store.lock();
        guard.record_scan_seen(&resume_op.id, &[clip.clone()]).unwrap();
    }
    fix.state.scan_coordinator.trigger_next(&fix.state);
    let resumed = fix.wait_scan_terminal_op(&resume_op.id).await;
    assert_eq!(resumed.status, store::ScanOperationStatus::Completed, "{:?}", resumed.report);
    assert_eq!(
        resumed.report["scanned"], 0,
        "断点命中的 Video 文件不得被重复处理: {}",
        resumed.report
    );
    assert!(
        fix.state.store().lock().get_file_meta(&ledger_id).unwrap().is_some(),
        "断点命中的文件不得被失效派生事实"
    );

    // 第三轮：内容变化后即使路径已进断点也必须重新处理并失效派生事实。
    let changed_op = enqueue(&fix, &library.id, &sig, &[]);
    {
        let store = fix.state.store();
        let guard = store.lock();
        guard.record_scan_seen(&changed_op.id, &[clip.clone()]).unwrap();
    }
    std::fs::write(&clip, b"v1-changed-content").unwrap();
    fix.state.scan_coordinator.trigger_next(&fix.state);
    let changed = fix.wait_scan_terminal_op(&changed_op.id).await;
    assert_eq!(changed.status, store::ScanOperationStatus::Completed, "{:?}", changed.report);
    assert_eq!(
        changed.report["scanned"], 1,
        "内容变化的 Video 文件必须重新处理: {}",
        changed.report
    );
    let store = fix.state.store();
    let guard = store.lock();
    assert!(
        guard.get_file_meta(&ledger_id).unwrap().is_none(),
        "变化 Video 文件的派生探测事实必须失效"
    );
    assert!(
        guard
            .scan_probe_obligations(100)
            .unwrap()
            .iter()
            .any(|obligation| obligation.ledger_id.to_string() == ledger_id),
        "变化 Video 文件必须重挂 durable probe 义务"
    );
}
