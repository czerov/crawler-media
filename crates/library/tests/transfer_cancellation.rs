use library::{TransferMode, WorkControl, transfer_file_controlled};

#[test]
fn cancelled_transfer_preserves_source_and_existing_destination() {
    for mode in [TransferMode::Copy, TransferMode::Move, TransferMode::Hardlink] {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.mkv");
        let destination = dir.path().join("destination.mkv");
        std::fs::write(&source, b"new").unwrap();
        std::fs::write(&destination, b"old").unwrap();
        let control = WorkControl::default();
        control.cancel();
        assert!(transfer_file_controlled(&source, &destination, mode, &control).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), b"new");
        assert_eq!(std::fs::read(&destination).unwrap(), b"old");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
