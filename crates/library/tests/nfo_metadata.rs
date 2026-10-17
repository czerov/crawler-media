use domain::{Media, MediaId, MediaKind};
use library::{CastMember, NfoMeta, parse_nfo, read_nfo, write_nfo, write_streamdetails_into_nfo};
use tempfile::tempdir;

const EMBY_TV_NFO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<TVSHOW>
  <title>PANTHEON</title>
  <originaltitle>Pantheon</originaltitle>
  <tagline>What is human?</tagline>
  <premiered>2022-09-01</premiered>
  <enddate>2025-02-21</enddate>
  <mpaa>TV-14</mpaa>
  <ratings>
    <rating name="themoviedb" max="10" default="true">
      <value>8.5</value>
      <votes>1234</votes>
    </rating>
    <rating name="imdb" max="10"><value>8.2</value><votes>900</votes></rating>
  </ratings>
  <original_language>en</original_language>
  <status>Ended</status>
  <country>United States</country>
  <studio>Netflix</studio>
  <director>Craig Silverstein</director>
  <creator>Craig Silverstein</creator>
  <seasoncount>2</seasoncount>
  <episodecount>16</episodecount>
  <actor>
    <name>Paul Dano</name><role>Caspian</role><tmdbid>4193</tmdbid>
    <thumb>https://image.tmdb.org/t/p/w185/a.jpg</thumb><order>4</order>
  </actor>
</TVSHOW>"#;

#[test]
fn parses_common_tv_metadata_and_nested_tmdb_rating() {
    let meta = parse_nfo(EMBY_TV_NFO).expect("valid TV NFO");

    assert_eq!(meta.original_title.as_deref(), Some("Pantheon"));
    assert_eq!(meta.tagline.as_deref(), Some("What is human?"));
    assert_eq!(meta.premiered.as_deref(), Some("2022-09-01"));
    assert_eq!(meta.end_date.as_deref(), Some("2025-02-21"));
    assert_eq!(meta.content_rating.as_deref(), Some("TV-14"));
    assert_eq!(meta.rating.as_deref(), Some("8.5"));
    assert_eq!(meta.vote_count, Some(1234));
    assert_eq!(meta.original_language.as_deref(), Some("en"));
    assert_eq!(meta.status.as_deref(), Some("Ended"));
    assert_eq!(meta.countries, vec!["United States"]);
    assert_eq!(meta.studios, vec!["Netflix"]);
    assert_eq!(meta.directors, vec!["Craig Silverstein"]);
    assert_eq!(meta.creators, vec!["Craig Silverstein"]);
    assert_eq!(meta.number_of_seasons, Some(2));
    assert_eq!(meta.number_of_episodes, Some(16));
    assert_eq!(meta.cast[0].name, "Paul Dano");
    assert_eq!(meta.cast[0].role.as_deref(), Some("Caspian"));
    assert_eq!(meta.cast[0].tmdb_id.as_deref(), Some("4193"));
    assert_eq!(
        meta.cast[0].thumb.as_deref(),
        Some("https://image.tmdb.org/t/p/w185/a.jpg")
    );
    assert_eq!(meta.cast[0].order, Some(4));
}

#[test]
fn parses_episode_aired_and_case_insensitive_root_and_tags() {
    let body = r#"<EPISODEDETAILS>
      <TITLE>Episode 1</TITLE><SEASON>0</SEASON><EPISODE>1</EPISODE>
      <AIRED>2025-03-04</AIRED>
    </EPISODEDETAILS>"#;

    let meta = parse_nfo(body).expect("valid episode NFO");
    assert_eq!(meta.title.as_deref(), Some("Episode 1"));
    assert_eq!(meta.season, Some(0));
    assert_eq!(meta.episode, Some(1));
    assert_eq!(meta.aired.as_deref(), Some("2025-03-04"));
}

#[test]
fn writer_round_trips_metadata_and_preserves_existing_streamdetails_and_unknown_fields() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("show.nfo");
    std::fs::write(
        &path,
        r#"<tvshow>
  <title>Old title</title>
  <plot>Existing plot</plot>
  <ratings><rating name="imdb"><value>7.9</value><votes>500</votes></rating>
    <rating name="themoviedb"><value>7.0</value><votes>100</votes></rating></ratings>
  <fileinfo><streamdetails><video><codec>hevc</codec></video></streamdetails></fileinfo>
  <customfield>preserve me</customfield>
</tvshow>"#,
    )
    .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Pantheon".into(),
        year: Some(2022),
        original_title: Some("Pantheon (Original)".into()),
        aliases: Vec::new(),
        tmdb_id: Some("999".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let meta = NfoMeta {
        original_title: Some("From NFO metadata".into()),
        tagline: Some("What is human?".into()),
        premiered: Some("2022-09-01".into()),
        end_date: Some("2025-02-21".into()),
        content_rating: Some("TV-14".into()),
        rating: Some("8.5".into()),
        vote_count: Some(1234),
        original_language: Some("en".into()),
        status: Some("Ended".into()),
        countries: vec!["United States".into()],
        studios: vec!["Netflix".into()],
        directors: vec!["Director".into()],
        creators: vec!["Creator".into()],
        number_of_seasons: Some(2),
        number_of_episodes: Some(16),
        cast: vec![CastMember {
            name: "Paul Dano".into(),
            role: Some("Caspian".into()),
            tmdb_id: Some("4193".into()),
            thumb: Some("https://image.tmdb.org/t/p/w185/a.jpg".into()),
            order: Some(4),
        }],
        ..Default::default()
    };

    write_nfo(&path, &media, Some(&meta)).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("<streamdetails>"));
    assert!(text.contains("<codec>hevc</codec>"));
    assert!(text.contains("<customfield>preserve me</customfield>"));
    assert!(text.contains("<originaltitle>From NFO metadata</originaltitle>"));
    assert!(text.contains("<tagline>What is human?</tagline>"));
    assert!(text.contains("<premiered>2022-09-01</premiered>"));
    assert!(text.contains("<enddate>2025-02-21</enddate>"));
    assert!(text.contains("<mpaa>TV-14</mpaa>"));
    assert!(text.contains("<votes>1234</votes>"));
    assert!(text.contains("name=\"imdb\""));
    assert!(text.contains("<value>7.9</value>"));
    assert!(text.contains("<order>4</order>"));

    let parsed = read_nfo(&path).expect("written NFO remains readable");
    assert_eq!(parsed.plot.as_deref(), Some("Existing plot"));
    assert_eq!(parsed.rating.as_deref(), Some("8.5"));
    assert_eq!(parsed.vote_count, Some(1234));
    assert_eq!(parsed.cast[0].order, Some(4));
}

#[test]
fn streamdetails_writer_handles_case_variants_without_duplicate_fileinfo() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("show.nfo");
    std::fs::write(&path, "<TVSHOW><title>Show</title></TVSHOW>").unwrap();

    write_streamdetails_into_nfo(&path, None, &[], &[]).unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("<fileinfo>"));
    assert!(written.contains("</TVSHOW>"));
    assert!(parse_nfo(&written).is_some());

    let uppercase_info = written
        .replace("<fileinfo>", "<FILEINFO>")
        .replace("</fileinfo>", "</FILEINFO>");
    std::fs::write(&path, &uppercase_info).unwrap();
    write_streamdetails_into_nfo(&path, None, &[], &[]).unwrap();
    let after_second_write = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after_second_write.matches("<FILEINFO>").count(), 1);
    assert_eq!(after_second_write.matches("<fileinfo>").count(), 0);
}

#[test]
fn nfo_writer_merges_only_supplied_fields() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("movie.nfo");
    std::fs::write(
        &path,
        "<movie><title>Old</title><plot>Keep plot</plot><studio>Keep studio</studio></movie>",
    )
    .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "New".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };

    write_nfo(&path, &media, None).unwrap();

    let parsed = read_nfo(&path).expect("merged NFO is readable");
    assert_eq!(parsed.title.as_deref(), Some("New"));
    assert_eq!(parsed.plot.as_deref(), Some("Keep plot"));
    assert_eq!(parsed.studios, vec!["Keep studio"]);
}
