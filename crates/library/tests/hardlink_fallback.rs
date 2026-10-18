//! D-M2: a failing hardlink either falls back to copy (counted) or, with the
//! fallback disabled, fails the transfer with a clear error.

use std::fs;

use library::{TransferMode, transfer_file};

#[test]
fn cross_device_hardlink_fallback_copy_counts_and_fail_mode_errors() {
    let shm = std::path::Path::new("/dev/shm");
    if !shm.is_dir() {
        eprintln!("no /dev/shm; skipped");
        return;
    }
    let src_dir = tempfile::tempdir_in(shm).unwrap();
    let lib_dir = tempfile::tempdir().unwrap();
    let src = src_dir.path().join("a.mkv");
    fs::write(&src, b"video").unwrap();

    let before = library::hardlink_fallback_count();
    transfer_file(
        &src,
        &lib_dir.path().join("copy/a.mkv"),
        TransferMode::Hardlink,
    )
    .unwrap();
    assert_eq!(
        library::hardlink_fallback_count(),
        before + 1,
        "fallback counted"
    );

    library::set_hardlink_fallback_copy(false);
    let err = transfer_file(
        &src,
        &lib_dir.path().join("fail/a.mkv"),
        TransferMode::Hardlink,
    )
    .unwrap_err()
    .to_string();
    library::set_hardlink_fallback_copy(true);
    assert!(err.contains("硬链接"), "{err}");
    assert!(!lib_dir.path().join("fail/a.mkv").exists());
}
