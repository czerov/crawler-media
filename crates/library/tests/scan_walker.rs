use std::time::Duration;

use domain::MediaKind;
use library::{ScanWalkBudget, ScanWalker, WorkControl};

#[test]
fn scan_walker_chunks_and_yields_on_entry_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();

    // Create 150 video files in root
    for i in 1..=150 {
        let f = root.join(format!("Movie_{i:03} (2024).mkv"));
        std::fs::write(&f, b"dummy").unwrap();
    }

    let budget = ScanWalkBudget {
        max_entries: 5000,
        max_elapsed: Duration::from_secs(10),
        chunk_files: 100, // chunk size 100
    };

    let control = WorkControl::default();
    let mut walker = ScanWalker::new(vec![root], MediaKind::Movie, control);

    // First chunk must contain exactly 100 files
    let chunk1 = walker.next_chunk(&budget).unwrap();
    assert_eq!(chunk1.files.len(), 100);
    assert!(chunk1.yielded);
    assert!(!chunk1.done);

    // Second chunk must contain remaining 50 files and report done
    let chunk2 = walker.next_chunk(&budget).unwrap();
    assert_eq!(chunk2.files.len(), 50);
    assert!(chunk2.done);
}

/// Video（「其他」库）语义：每个视频文件一个条目，不做文件名识别，也不因解析
/// 置信度低而丢弃（旧递归扫描就是这个语义，不能被共用 walker 改掉）。
#[test]
fn scan_walker_video_kind_keeps_low_confidence_names() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("homevideos");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("clip.mp4"), b"v").unwrap();
    std::fs::write(root.join("宝宝满月纪念.mp4"), b"v").unwrap();
    std::fs::write(root.join("notes.txt"), b"n").unwrap();

    let budget = ScanWalkBudget::default();
    let mut walker = ScanWalker::new(vec![root.clone()], MediaKind::Video, WorkControl::default());
    let chunk = walker.next_chunk(&budget).unwrap();
    assert!(chunk.done);
    assert_eq!(chunk.files.len(), 2, "Video 库必须保留所有视频文件");
    assert_eq!(chunk.skipped, 1, "非视频文件必须计入 skipped");
    assert!(
        chunk.files.iter().all(|file| file.identified_release.is_none()),
        "Video 条目不做 Release 识别"
    );

    // 同样的目录在 Movie 语义下会因文件名置信度低而全部跳过——证明这是 Video 专属钩子。
    let mut movie_walker = ScanWalker::new(vec![root], MediaKind::Movie, WorkControl::default());
    let movie_chunk = movie_walker.next_chunk(&budget).unwrap();
    assert!(
        movie_chunk.files.is_empty(),
        "Movie 语义仍须丢弃低置信度文件名: {:?}",
        movie_chunk.files
    );
}

/// 取消必须能在下一个目录项/下一个 chunk 之前生效，而不是走完整棵树。
#[test]
fn scan_walker_cancelled_control_stops_iteration() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("homevideos");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("clip.mp4"), b"v").unwrap();

    let control = WorkControl::default();
    control.cancel();
    let mut walker = ScanWalker::new(vec![root], MediaKind::Video, control);
    let result = walker.next_chunk(&ScanWalkBudget::default());
    assert!(
        matches!(result, Err(library::LibraryError::Cancelled)),
        "取消的 work control 必须让遍历返回 Cancelled"
    );
}
