use super::*;

struct ReservationRecorder(std::cell::RefCell<Option<domain::LedgerRow>>);

impl subscribe::TransferRecorder for ReservationRecorder {
    fn prepare(&self, row: &domain::LedgerRow, _: &Path, _: TransferMode)
        -> Result<domain::LedgerRow, subscribe::SubscribeError>
    {
        assert!(!Path::new(&row.path).exists(), "reservation must precede publication");
        *self.0.borrow_mut() = Some(row.clone());
        Ok(row.clone())
    }

    fn published(&self, row: &domain::LedgerRow) -> Result<(), subscribe::SubscribeError> {
        assert!(Path::new(&row.path).is_file());
        assert_eq!(self.0.borrow().as_ref().unwrap().id, row.id);
        Ok(())
    }
}

#[test]
fn recorded_collection_reserves_intended_path_before_publication() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, false);
    let torrent = torrent("The.Matrix.1999.1080p.BluRay.x264-GROUP", "recorded");
    let source = tmp.path().join("The.Matrix.1999.1080p.mkv");
    fs::write(&source, b"video").unwrap();
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    map_file(&dl, &torrent, source);
    let root = tmp.path().join("lib");
    let added = subscribe::admit_and_add(run_input(&sub, &media, &filter, vec![torrent],
        SubscribeFacts::default(), &dl, &root)).unwrap();
    let recorder = ReservationRecorder(Default::default());
    let outcome = subscribe::collect_completed_recorded(added, &FixedProbe, &[],
        &library::WorkControl::default(), Some(&recorder)).unwrap();
    assert_eq!(outcome.ledger.len(), 1);
    assert_eq!(outcome.ledger[0].path, recorder.0.borrow().as_ref().unwrap().path);
    assert!(outcome.collection_errors.is_empty());
}

#[test]
fn pending_publication_retry_reuses_reserved_destination() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, false);
    let torrent = torrent("The.Matrix.1999.1080p.BluRay.x264-GROUP", "pending");
    let source = tmp.path().join("The.Matrix.1999.1080p.mkv");
    fs::write(&source, b"video").unwrap();
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    map_file(&dl, &torrent, source.clone());
    let root = tmp.path().join("lib");
    let destination = root.join("reserved.mkv");
    let mut mapping = subscribe::collection_destinations::DestinationMapping::new(
        tmp.path().join("stage").join(source.file_name().unwrap()).display().to_string(), &destination);
    mapping.transfer_pending = true;
    let added = subscribe::admit_and_add(run_input(&sub, &media, &filter, vec![torrent],
        SubscribeFacts::default(), &dl, &root)).unwrap();
    let recorder = ReservationRecorder(Default::default());
    let outcome = subscribe::collect_completed_recorded(added, &FixedProbe, &[mapping],
        &library::WorkControl::default(), Some(&recorder)).unwrap();
    assert!(outcome.collection_errors.is_empty());
    assert_eq!(outcome.ledger.len(), 1);
    assert_eq!(outcome.ledger[0].path, destination.display().to_string());
    assert_eq!(fs::read_dir(root).unwrap().count(), 1);
}

#[test]
fn transfer_ledger_row_uses_probed_quality() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, false);
    let t = torrent(
        "The.Matrix.1999.1080p.BluRay.x264-GROUP",
        "https://pt.example/dl/1",
    );
    let src = tmp.path().join("probed.mkv");
    write_probed(&src, "2160p", "hevc", "hdr10");
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    map_file(&dl, &t, src);

    let outcome = run_with_probe(
        run_input(
            &sub,
            &media,
            &filter,
            vec![t],
            SubscribeFacts::default(),
            &dl,
            &tmp.path().join("lib"),
        ),
        &FixedProbe,
    )
    .unwrap();

    let row = &outcome.ledger[0];
    assert_eq!(row.media_id, media.id);
    assert!(Path::new(&row.path).exists());
    assert_eq!(row.resolution.as_deref(), Some("2160p"));
    assert_eq!(row.codec.as_deref(), Some("hevc"));
    assert_eq!(row.hdr.as_deref(), Some("hdr10"));
    assert_eq!(row.quality_source, domain::QualitySource::Probe);
    assert!(row.season.is_none());
    assert!(row.episode.is_none());
}

#[test]
fn transfer_falls_back_to_release_when_probe_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, false);
    let t = torrent(
        "The.Matrix.1999.1080p.BluRay.x264-GROUP",
        "https://pt.example/dl/1",
    );
    let src = tmp.path().join("opaque.mkv");
    fs::write(&src, b"\x00\x01not-json").unwrap();
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    map_file(&dl, &t, src);

    let outcome = run(run_input(
        &sub,
        &media,
        &filter,
        vec![t],
        SubscribeFacts::default(),
        &dl,
        &tmp.path().join("lib"),
    ))
    .unwrap();

    let row = &outcome.ledger[0];
    assert_eq!(row.resolution.as_deref(), Some("1080p"));
    assert_eq!(row.quality_source, domain::QualitySource::Release);
    assert_eq!(row.confidence, domain::Confidence::Low);
}
#[test]
fn extra_files_do_not_overwrite_the_main_library_file() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, false);
    let t = torrent(
        "The.Matrix.1999.2160p.BluRay.x265-GROUP",
        "https://pt.example/dl/extras",
    );
    let feature = tmp
        .path()
        .join("The.Matrix.1999.2160p.BluRay.x265-GROUP.mkv");
    let sample = tmp.path().join("sample.mkv");
    fs::write(&feature, b"feature").unwrap();
    fs::write(&sample, b"sample").unwrap();
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    dl.map_enclosure_files(&t.enclosure, vec![feature, sample]);

    let outcome = run(run_input(
        &sub,
        &media,
        &filter,
        vec![t],
        SubscribeFacts::default(),
        &dl,
        &tmp.path().join("lib"),
    ))
    .unwrap();

    assert_eq!(outcome.ledger.len(), 2);
    let mut names: Vec<_> = outcome
        .ledger
        .iter()
        .map(|row| {
            Path::new(&row.path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(names.len(), 2);
    assert_ne!(names[0], names[1]);
    for name in &names {
        assert!(name.ends_with(".mkv"));
        assert!(!name.contains("BluRay"));
    }
    for row in &outcome.ledger {
        assert!(Path::new(&row.path).is_file());
    }
    assert_ne!(
        fs::read(&outcome.ledger[0].path).unwrap(),
        fs::read(&outcome.ledger[1].path).unwrap()
    );
}
