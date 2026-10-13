use std::path::PathBuf;
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
