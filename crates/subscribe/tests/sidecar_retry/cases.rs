// Behavioral regression cases included by sidecar_retry.rs.
use subscribe::collection_destinations::DestinationMapping;

struct RetryFixture {
    media: Media,
    sub: domain::Subscribe,
    filter: domain::Filter,
    torrent: Torrent,
    root: PathBuf,
    naming: String,
}

impl RetryFixture {
    fn movie(root: PathBuf) -> Self {
        let (media, sub, filter, torrent) = make_movie_fixtures();
        Self { media, sub, filter, torrent, root, naming: "{title} ({year})/{title} ({year}){ext}".into() }
    }

    fn round(&self, files: Vec<PathBuf>, facts: SubscribeFacts, mappings: &[DestinationMapping]) -> subscribe::RunOutcome {
        let dl = MockDownloader { files };
        let input = RunInput {
            subscribe: &self.sub, media: &self.media, filter: &self.filter, wash_filter: None,
            torrents: vec![], facts, search_keywords: vec![], downloader: &dl,
            library_root: &self.root, transfer_mode: Some(library::TransferMode::Copy),
            scrape: false, hooks: None, naming: Some(&self.naming), preserve_removed: true,
            overlap: Default::default(),
            absolute: None,
        };
        let added = make_added(input, self.torrent.clone(), release::parse(&self.torrent.title));
        collect_completed_with_destinations(added, &library::Ffprobe::default(), mappings).unwrap()
    }
}

fn mappings(outcome: &subscribe::RunOutcome) -> Vec<DestinationMapping> {
    outcome.ledger_sources.iter().map(|s| DestinationMapping::new(&s.source_path, &s.ledger_path)).collect()
}

#[test]
fn movie_retry_uses_persisted_destination_and_finishes_without_new_video() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = RetryFixture::movie(tmp.path().join("library"));
    let video = tmp.path().join("Movie.1999.1080p.mkv");
    let sub = tmp.path().join("Movie.1999.1080p.zh.srt");
    std::fs::write(&video, b"video").unwrap();
    std::fs::write(&sub, b"subtitle").unwrap();
    let blocker = fixture.root.join("Movie (1999)/Movie (1999).zh.srt");
    std::fs::create_dir_all(&blocker).unwrap();
    let first = fixture.round(vec![video.clone(), sub.clone()], SubscribeFacts::default(), &[]);
    assert_eq!(first.ledger.len(), 1);
    assert!(!first.collection_errors.is_empty());
    assert!(first.transferred_enclosures.is_empty());
    let persisted = mappings(&first);
    assert_eq!(blocker.canonicalize().unwrap(), fixture.root.canonicalize().unwrap().join("Movie (1999)/Movie (1999).zh.srt"));
    std::fs::remove_dir(&blocker).unwrap();
    let second = fixture.round(vec![video.clone(), sub], first.facts, &persisted);
    assert!(second.collection_errors.is_empty());
    assert!(second.ledger.is_empty());
    assert_eq!(second.transferred_enclosures.len(), 1);
    assert_eq!(std::fs::read(&blocker).unwrap(), b"subtitle");
    let third = fixture.round(vec![video], second.facts, &persisted);
    assert!(third.ledger.is_empty());
    assert_eq!(third.transferred_enclosures.len(), 1, "owned video without subtitles still completes");
}

#[test]
fn mixed_tv_retry_matches_source_even_when_destinations_have_no_release_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fixture = RetryFixture::movie(tmp.path().join("library"));
    fixture.media.kind = MediaKind::Tv;
    fixture.media.title = "Long Watch".into();
    fixture.media.year = None;
    fixture.sub.coverage = domain::Coverage::Tv { season: 1, episode_from: 1, episode_to: Some(2) };
    fixture.torrent.title = "Long.Watch.S01E01-E02.1080p".into();
    fixture.naming = "{title}/{season}/{episode}{ext}".into();
    let e1 = tmp.path().join("Long.Watch.S01E01.1080p.mkv");
    let e2 = tmp.path().join("Long.Watch.S01E02.1080p.mkv");
    let subtitle = tmp.path().join("Long.Watch.S01E01.1080p.zh.srt");
    for path in [&e1, &e2] { std::fs::write(path, b"video").unwrap(); }
    std::fs::write(&subtitle, b"E1 subtitle").unwrap();
    let blocker = fixture.root.join("Long Watch/01/01.zh.srt");
    std::fs::create_dir_all(&blocker).unwrap();
    let first = fixture.round(vec![e1.clone(), subtitle.clone()], SubscribeFacts::default(), &[]);
    assert_eq!(first.ledger.len(), 1);
    assert!(!first.collection_errors.is_empty());
    let persisted = mappings(&first);
    assert!(blocker.canonicalize().unwrap().starts_with(fixture.root.canonicalize().unwrap()));
    std::fs::remove_dir(&blocker).unwrap();
    let second = fixture.round(vec![e1, e2, subtitle], first.facts, &persisted);
    assert!(second.collection_errors.is_empty(), "{:?}", second.collection_errors);
    assert_eq!(second.ledger.len(), 1);
    assert_eq!(second.transferred_enclosures.len(), 1);
    assert_eq!(std::fs::read(blocker).unwrap(), b"E1 subtitle");
    assert!(!fixture.root.join("Long Watch/01/02.zh.srt").exists());
}
