use domain::{Media, MediaId, MediaKind};
use library::{WatchJob, WatchKind, WorkControl, scan_watch_intake_controlled};

fn fixture() -> (tempfile::TempDir, WatchJob, Media) {
    let temp = tempfile::tempdir().unwrap();
    let intake = temp.path().join("intake");
    std::fs::create_dir_all(&intake).unwrap();
    let job = WatchJob { kind: WatchKind::Intake, path: intake,
        library_root: temp.path().join("movies"), tv_library_root: Some(temp.path().join("tv")), scrape: true };
    let media = Media { id: MediaId::new(), kind: MediaKind::Movie, title: "Fixture".into(),
        year: None, original_title: None, aliases: Vec::new(), tmdb_id: Some("123".into()), douban_id: None,
        tvdb_id: None, bangumi_id: None, anilist_id: None };
    (temp, job, media)
}

#[test]
fn cancelled_movie_intake_preserves_source_and_creates_no_destination() {
    let (_temp, job, media) = fixture();
    let src = job.path.join("Fixture.2022.mkv");
    std::fs::write(&src, b"source").unwrap();
    let movie = WorkControl::default();
    movie.cancel();
    assert!(scan_watch_intake_controlled(&job, &media, &movie, None).is_err());
    assert_eq!(std::fs::read(src).unwrap(), b"source");
    assert!(!job.library_root.exists());
}

#[test]
fn tv_never_falls_back_to_movie_without_a_destination_control() {
    let (_temp, job, media) = fixture();
    let name = "Fixture.S01E01.mkv";
    std::fs::write(job.path.join(name), b"source").unwrap();
    assert!(scan_watch_intake_controlled(&job, &media, &WorkControl::default(), None).is_err());
    assert!(!job.library_root.join(name).exists());
    assert!(!job.tv_library_root.unwrap().join(name).exists());
}

#[test]
fn active_tv_control_routes_to_tv_destination() {
    let (_temp, job, media) = fixture();
    let name = "Fixture.S01E01.mkv";
    std::fs::write(job.path.join(name), b"source").unwrap();
    let outcome = scan_watch_intake_controlled(&job, &media, &WorkControl::default(), Some(&WorkControl::default())).unwrap();
    assert_eq!(outcome.transferred.len(), 1);
    assert_eq!(std::fs::read(job.tv_library_root.unwrap().join(name)).unwrap(), b"source");
    assert!(!job.library_root.join(name).exists());
}
