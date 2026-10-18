use std::path::Path;

use domain::{Confidence, Media, MediaId, MediaKind, Release};
use library::{default_pattern, render_path, validate_pattern};

fn movie() -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: Some(1999),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn tv() -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "The Expanse".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn release_movie() -> Release {
    Release {
        title: "The Matrix".into(),
        year: Some(1999),
        season: None,
        episode: None,
        episode_to: None,
        resolution: Some("2160p".into()),
        source: Some("BluRay".into()),
        codec: Some("x265".into()),
        hdr: None,
        subtitle_language: None,
        audio_language: None,
        group: None,
        confidence: Confidence::High,
        season_inferred: false,
    }
}

fn release_ep() -> Release {
    Release {
        title: "The Expanse".into(),
        year: None,
        season: Some(1),
        episode: Some(1),
        episode_to: None,
        resolution: Some("1080p".into()),
        source: None,
        codec: None,
        hdr: None,
        subtitle_language: None,
        audio_language: None,
        group: None,
        confidence: Confidence::High,
        season_inferred: false,
    }
}

#[test]
fn movie_pattern_includes_year_and_resolution() {
    let dest = render_path(
        Path::new("/lib"),
        default_pattern(MediaKind::Movie),
        &movie(),
        &release_movie(),
        Path::new("/dl/source.mkv"),
    )
    .unwrap();
    assert_eq!(
        dest,
        Path::new("/lib/The Matrix (1999)/The Matrix (1999) - 2160p.mkv")
    );
}

#[test]
fn missing_year_does_not_leave_empty_parens() {
    let mut media = movie();
    media.year = None;
    let mut release = release_movie();
    release.year = None;
    let dest = render_path(
        Path::new("/lib"),
        default_pattern(MediaKind::Movie),
        &media,
        &release,
        Path::new("a.mkv"),
    )
    .unwrap();
    let text = dest.to_string_lossy();
    assert!(!text.contains("()"), "{text}");
    assert!(text.contains("The Matrix"));
}

#[test]
fn tv_pattern_uses_season_folder_and_season_episode() {
    let dest = render_path(
        Path::new("/lib"),
        default_pattern(MediaKind::Tv),
        &tv(),
        &release_ep(),
        Path::new("ep.mkv"),
    )
    .unwrap();
    assert_eq!(
        dest,
        Path::new("/lib/The Expanse/Season 01/The Expanse - S01E01.mkv")
    );
}

#[test]
fn unknown_placeholder_is_a_config_error() {
    let err = validate_pattern("{title} {nope}").unwrap_err();
    assert!(err.to_string().contains("nope"));
}

#[test]
fn extended_tokens_render_and_shrink() {
    let media = Media {
        original_title: Some("The Matrix".into()),
        aliases: Vec::new(),
        tmdb_id: Some("603".into()),
        ..movie()
    };
    let release = Release {
        group: Some("OurTV".into()),
        ..release_movie()
    };
    let rendered = render_path(
        Path::new("/root"),
        "[tmdbid-{tmdb_id}]{original_title} ({year}) - {release_group}{ext}",
        &media,
        &release,
        Path::new("x.mkv"),
    )
    .unwrap();
    assert_eq!(
        rendered.to_string_lossy(),
        "/root/[tmdbid-603]The Matrix (1999) - OurTV.mkv"
    );

    // 缺值的占位符整组收缩：`[imdbid-{imdb_id}]` 不残留 `[imdbid-]`。
    let rendered = render_path(
        Path::new("/root"),
        "[imdbid-{imdb_id}] {title}",
        &media,
        &release,
        Path::new("x.mkv"),
    )
    .unwrap();
    assert_eq!(rendered.to_string_lossy(), "/root/The Matrix");
}

#[test]
fn pad_suffix_zero_pads_season_and_episode() {
    let release = Release {
        season: Some(1),
        episode: Some(3),
        ..release_movie()
    };
    let rendered = render_path(
        Path::new("/"),
        "S{season:02d}E{episode:02d}{ext}",
        &tv(),
        &release,
        Path::new("x.mkv"),
    )
    .unwrap();
    assert_eq!(rendered.to_string_lossy(), "/S01E03.mkv");
}

#[test]
fn validate_rejects_unknown_token_and_bad_pad() {
    assert!(validate_pattern("{title} {nope}").is_err());
    assert!(validate_pattern("{title:99d}").is_err());
    assert!(
        validate_pattern("{title:02d}").is_err(),
        "pad only for numeric tokens"
    );
    assert!(validate_pattern("{season:02d}").is_ok());
}

#[test]
fn default_patterns_render_byte_identical() {
    let movie_path = render_path(
        Path::new("/lib"),
        default_pattern(MediaKind::Movie),
        &movie(),
        &release_movie(),
        Path::new("The.Matrix.1999.2160p.mkv"),
    )
    .unwrap();
    assert_eq!(
        movie_path.to_string_lossy(),
        "/lib/The Matrix (1999)/The Matrix (1999) - 2160p.mkv"
    );
}
