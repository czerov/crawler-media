use library::{TransferMode, WorkControl};
use subscribe::{
    collection_destinations::DestinationMapping, sidecars::place_mapped_subtitle_controlled,
};

#[test]
fn cancelled_subtitle_retry_preserves_source_and_owned_sidecar() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("Movie.2024.en.srt");
    let video_source = tmp.path().join("Movie.2024.mkv");
    let video_dest = tmp.path().join("library/Movie.2024.mkv");
    let subtitle_dest = video_dest.with_extension("en.srt");
    std::fs::create_dir_all(video_dest.parent().unwrap()).unwrap();
    std::fs::write(&source, b"new subtitle").unwrap();
    std::fs::write(&subtitle_dest, b"owned subtitle").unwrap();
    let mappings = [DestinationMapping::new(
        video_source.display().to_string(),
        video_dest,
    )];
    let control = WorkControl::default();
    control.cancel();
    assert!(
        place_mapped_subtitle_controlled(&source, &mappings, Some(TransferMode::Move), &control)
            .is_err()
    );
    assert_eq!(std::fs::read(source).unwrap(), b"new subtitle");
    assert_eq!(std::fs::read(subtitle_dest).unwrap(), b"owned subtitle");
}
