#[test]
fn season_nfo_existing_dir_does_not_create_missing_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("gone");
    let error = library::write_season_nfo_in_existing_dir(
        &root.join("season.nfo"),
        &library::NfoMeta::default(),
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(!root.exists());
}

#[test]
fn season_nfo_existing_dir_writes_valid_season_nfo() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("exists");
    std::fs::create_dir(&root).unwrap();
    let file = root.join("season.nfo");
    library::write_season_nfo_in_existing_dir(
        &file,
        &library::NfoMeta {
            title: Some("Season 1".into()),
            season: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    let content = std::fs::read_to_string(&file).unwrap();
    assert!(content.contains("<title>Season 1</title>"));
    assert!(content.contains("<seasonnumber>1</seasonnumber>"));
}

#[test]
fn season_nfo_original_writer_still_creates_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("created");
    let file = root.join("season.nfo");
    library::write_season_nfo(
        &file,
        &library::NfoMeta {
            title: Some("Season 1".into()),
            season: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(root.exists());
    assert!(file.is_file());
}
