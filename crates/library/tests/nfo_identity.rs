use domain::{Media, MediaId, MediaKind};
use library::{WatchJob, WatchKind, parse_nfo_identity, read_watch_identity, scan_watch};

fn media() -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "fixture".into(),
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

#[test]
fn parses_work_aliases_but_never_episode_identity() {
    let identity = parse_nfo_identity(r#"<tvshow><title>动灵守护者</title><year>2022</year><uniqueid type="tmdb">207890</uniqueid></tvshow>"#).unwrap();
    assert_eq!(identity.kind, MediaKind::Tv);
    assert_eq!(identity.tmdb_id.as_deref(), Some("207890"));
    assert_eq!(identity.title.as_deref(), Some("动灵守护者"));
    assert!(
        parse_nfo_identity(
            "<episodedetails><title>Episode</title><tmdbid>999</tmdbid></episodedetails>"
        )
        .is_none()
    );
    let movie = parse_nfo_identity("<movie><tmdbid>207890</tmdbid><id>555</id></movie>").unwrap();
    assert_eq!(movie.kind, MediaKind::Movie);
    assert_eq!(movie.tmdb_id.as_deref(), Some("207890"));
    assert!(
        parse_nfo_identity("<movie><id>555</id></movie>")
            .unwrap()
            .tmdb_id
            .is_none()
    );
}

#[test]
fn rejects_invalid_and_conflicting_aliases() {
    for xml in [
        "<movie><tmdbid>0</tmdbid></movie>",
        "<movie><tmdbid>-1</tmdbid></movie>",
        "<movie><tmdbid>abc</tmdbid></movie>",
        "<movie><tmdbid></tmdbid></movie>",
        "<movie><tmdbid>12</tmdbid><uniqueid type=\"tmdb\">13</uniqueid></movie>",
        "<movie><title>broken</movie>",
    ] {
        assert!(parse_nfo_identity(xml).is_none(), "{xml}");
    }
}

#[test]
fn scan_uses_tv_work_over_high_confidence_filename_and_episode_fields() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("Season 1");
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("English.Name.2025.S01E03.strm");
    std::fs::write(&path, "https://invalid.example/video").unwrap();
    std::fs::write(
        temp.path().join("tvshow.nfo"),
        "<tvshow><title>中文作品</title><year>2022</year><tmdbid>207890</tmdbid></tvshow>",
    )
    .unwrap();
    std::fs::write(path.with_extension("nfo"), "<episodedetails><title>单集标题</title><year>2026</year><season>2</season><episode>4</episode><tmdbid>999</tmdbid></episodedetails>").unwrap();
    let outcome = scan_watch(
        &WatchJob {
            kind: WatchKind::InPlace,
            path: temp.path().into(),
            library_root: temp.path().into(),
            tv_library_root: None,
            scrape: false,
        },
        &media(),
    )
    .unwrap();
    let file = &outcome.transferred[0];
    let release = file.identified_release.as_ref().unwrap();
    assert_eq!(release.title, "中文作品");
    assert_eq!(release.year, Some(2022));
    assert_eq!((release.season, release.episode), (Some(1), Some(3)));
    assert_eq!(
        file.nfo_identity.as_ref().unwrap().tmdb_id.as_deref(),
        Some("207890")
    );
}

#[test]
fn movie_lookup_never_uses_unrelated_sibling_or_parent_nfo() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("child");
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("movie.strm");
    std::fs::write(&path, "fixture").unwrap();
    std::fs::write(
        dir.join("unrelated.nfo"),
        "<movie><tmdbid>1</tmdbid></movie>",
    )
    .unwrap();
    std::fs::write(
        temp.path().join("movie.nfo"),
        "<movie><tmdbid>2</tmdbid></movie>",
    )
    .unwrap();
    assert!(
        read_watch_identity(&path, &dir, MediaKind::Movie)
            .unwrap()
            .is_none()
    );
    std::fs::write(
        path.with_extension("nfo"),
        "<movie><tmdbid>3</tmdbid></movie>",
    )
    .unwrap();
    assert_eq!(
        read_watch_identity(&path, &dir, MediaKind::Movie)
            .unwrap()
            .unwrap()
            .tmdb_id
            .as_deref(),
        Some("3")
    );
    assert!(
        read_watch_identity(&path, &dir, MediaKind::Tv)
            .unwrap()
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn refuses_sidecar_and_directory_symlinks_outside_root() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        outside.path().join("tvshow.nfo"),
        "<tvshow><tmdbid>1</tmdbid></tvshow>",
    )
    .unwrap();
    std::fs::write(outside.path().join("episode.strm"), "fixture").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
    assert!(
        read_watch_identity(
            &root.path().join("escape/episode.strm"),
            root.path(),
            MediaKind::Tv
        )
        .unwrap()
        .is_none()
    );
    std::fs::write(root.path().join("episode.strm"), "fixture").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("tvshow.nfo"),
        root.path().join("tvshow.nfo"),
    )
    .unwrap();
    assert!(
        read_watch_identity(
            &root.path().join("episode.strm"),
            root.path(),
            MediaKind::Tv
        )
        .unwrap()
        .is_none()
    );
}
