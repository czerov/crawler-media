use std::fs;

use library::{TransferMode, transfer_file};

#[test]
fn hardlink_replacement_preserves_old_seeding_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let old_source = tmp.path().join("old-seeding.mkv");
    let new_source = tmp.path().join("new-seeding.mkv");
    let destination = tmp.path().join("library/movie.mkv");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&old_source, b"old release bytes").unwrap();
    fs::write(&new_source, b"new release bytes").unwrap();
    fs::hard_link(&old_source, &destination).unwrap();

    // D-H3: publication never overwrites an existing destination; the old
    // seeding bytes (shared via hardlink) are never written through.
    let error = transfer_file(&new_source, &destination, TransferMode::Hardlink).unwrap_err();
    assert!(
        matches!(&error, library::LibraryError::Io(e) if e.kind() == std::io::ErrorKind::AlreadyExists),
        "{error:?}"
    );

    assert_eq!(fs::read(&destination).unwrap(), b"old release bytes");
    assert_eq!(fs::read(&old_source).unwrap(), b"old release bytes");
    assert_eq!(fs::read(&new_source).unwrap(), b"new release bytes");
    assert_eq!(fs::read_dir(destination.parent().unwrap()).unwrap().count(), 1, "no temp left behind");
}

#[test]
fn transfer_never_overwrites_existing_destination_in_any_mode() {
    for mode in [TransferMode::Copy, TransferMode::Move, TransferMode::Hardlink] {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("new.mkv");
        let dest = tmp.path().join("library/movie.mkv");
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(&src, b"new").unwrap();
        fs::write(&dest, b"user file").unwrap();
        let error = transfer_file(&src, &dest, mode).unwrap_err();
        assert!(matches!(&error, library::LibraryError::Io(e) if e.kind() == std::io::ErrorKind::AlreadyExists), "{mode:?}: {error:?}");
        assert_eq!(fs::read(&dest).unwrap(), b"user file", "{mode:?}");
        assert!(src.is_file(), "{mode:?}: source kept on conflict");
        assert_eq!(fs::read_dir(dest.parent().unwrap()).unwrap().count(), 1, "{mode:?}: no temp left");
    }
}

#[test]
fn concurrent_transfers_to_one_destination_publish_exactly_one() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("library/movie.mkv");
    let sources: Vec<_> = (0..8)
        .map(|i| {
            let src = tmp.path().join(format!("src-{i}.mkv"));
            fs::write(&src, vec![b'a' + i as u8; 64 * 1024]).unwrap();
            src
        })
        .collect();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(sources.len()));
    let handles: Vec<_> = sources
        .iter()
        .cloned()
        .map(|src| {
            let dest = dest.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                transfer_file(&src, &dest, TransferMode::Copy).is_ok()
            })
        })
        .collect();
    let wins = handles.into_iter().map(|h| h.join().unwrap()).filter(|ok| *ok).count();
    assert_eq!(wins, 1, "exactly one writer publishes");
    let bytes = fs::read(&dest).unwrap();
    assert!(bytes.iter().all(|b| *b == bytes[0]), "published file is one intact source");
    assert_eq!(fs::read_dir(dest.parent().unwrap()).unwrap().count(), 1, "no temps left");
}

#[test]
fn transfer_to_a_hardlink_of_itself_is_a_no_op() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("seed.mkv");
    let dest = tmp.path().join("library/movie.mkv");
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(&src, b"bytes").unwrap();
    fs::hard_link(&src, &dest).unwrap();
    transfer_file(&src, &dest, TransferMode::Hardlink).unwrap();
    assert_eq!(fs::read(&dest).unwrap(), b"bytes");
}

#[test]
fn same_filesystem_default_is_hardlink_before_library_root_exists() {
    use std::os::unix::fs::MetadataExt;

    use library::resolve_mode;

    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source.mkv");
    let root = tmp.path().join("new/library/root");
    let destination = root.join("movie.mkv");
    fs::write(&source, b"bytes").unwrap();

    let mode = resolve_mode(&source, &root, None);
    transfer_file(&source, &destination, mode).unwrap();

    assert_eq!(
        fs::metadata(source).unwrap().ino(),
        fs::metadata(destination).unwrap().ino()
    );
}

#[test]
fn move_failure_preserves_source_file_without_data_loss() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source-precious.mkv");
    fs::write(&source, b"precious media content").unwrap();

    // 目标路径被预先创建为一个同名目录，导致向其发布时必然产生冲突与重命名失败
    let destination = tmp.path().join("conflict_dest");
    fs::create_dir_all(&destination).unwrap();

    let res = transfer_file(&source, &destination, TransferMode::Move);
    assert!(res.is_err(), "向目录发布文件必须返回失败错误");

    // 关键断言：源文件必须依然完好存在，绝对不能发生数据丢失！
    assert!(source.exists(), "转移发布失败时源文件必须被安全保留在原位");
    assert_eq!(fs::read(&source).unwrap(), b"precious media content");
}

#[cfg(unix)]
#[test]
fn same_device_move_is_a_rename_without_copy() {
    use std::os::unix::fs::MetadataExt;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("dl/a.mkv");
    let dest = dir.path().join("lib/a.mkv");
    fs::create_dir_all(src.parent().unwrap()).unwrap();
    fs::write(&src, b"video").unwrap();
    let inode = fs::metadata(&src).unwrap().ino();
    transfer_file(&src, &dest, TransferMode::Move).unwrap();
    assert!(!src.exists(), "source moved away");
    assert_eq!(fs::metadata(&dest).unwrap().ino(), inode, "same inode: no copy");
    assert_eq!(fs::read(&dest).unwrap(), b"video");
}
