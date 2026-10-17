use std::path::Path;

use domain::{Media, MediaId, MediaKind};
use library::{NfoMeta, parse_nfo, read_nfo, write_nfo};

const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<movie>
  <title>The Matrix</title>
  <year>1999</year>
  <plot>Morpheus offers Neo the red pill.</plot>
  <rating>8.7</rating>
  <runtime>136</runtime>
  <genre>Action</genre>
  <genre>Sci-Fi</genre>
  <actor>
    <name>Keanu Reeves</name>
    <role>Neo</role>
  </actor>
  <actor>
    <name>Laurence Fishburne</name>
    <role>Morpheus</role>
  </actor>
</movie>
"#;

#[test]
fn parses_emby_style_movie_nfo() {
    let meta = parse_nfo(SAMPLE).expect("valid nfo");
    assert_eq!(meta.title.as_deref(), Some("The Matrix"));
    assert_eq!(meta.year.as_deref(), Some("1999"));
    assert_eq!(
        meta.plot.as_deref(),
        Some("Morpheus offers Neo the red pill.")
    );
    assert_eq!(meta.rating.as_deref(), Some("8.7"));
    assert_eq!(meta.runtime_minutes.as_deref(), Some("136"));
    assert_eq!(meta.genres, vec!["Action", "Sci-Fi"]);
    assert_eq!(meta.cast.len(), 2);
    assert_eq!(meta.cast[0].name, "Keanu Reeves");
    assert_eq!(meta.cast[0].role.as_deref(), Some("Neo"));
}

#[test]
fn rejects_non_nfo_input() {
    assert!(parse_nfo("<html><body>nope</body></html>").is_none());
}

#[test]
fn write_then_read_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("movie.nfo");
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: Some(1999),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let meta = NfoMeta {
        plot: Some("A hacker discovers the truth.".into()),
        rating: Some("8.7".into()),
        runtime_minutes: Some("136".into()),
        genres: vec!["Action".into()],
        cast: vec![library::CastMember {
            name: "Keanu Reeves".into(),
            role: Some("Neo".into()),
            tmdb_id: Some("6384".into()),
            thumb: None,
            order: None,
        }],
        ..Default::default()
    };
    write_nfo(&path, &media, Some(&meta)).unwrap();
    let parsed = read_nfo(&path).expect("round trip");
    assert_eq!(
        parsed.plot.as_deref(),
        Some("A hacker discovers the truth.")
    );
    assert_eq!(parsed.genres, vec!["Action"]);
    assert_eq!(parsed.cast[0].name, "Keanu Reeves");
    assert!(path.to_string_lossy().contains("movie.nfo"));
}

#[test]
fn missing_file_reads_none() {
    assert!(read_nfo(Path::new("/nonexistent/x.nfo")).is_none());
}
