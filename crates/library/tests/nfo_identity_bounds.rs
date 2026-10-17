use domain::{Media, MediaId, MediaKind};
use library::{WatchJob, WatchKind, parse_nfo_identity, read_watch_identity, scan_watch};

#[test]
fn rejects_dtd_and_oversized_xml_without_expansion() {
    assert!(
        parse_nfo_identity(
            "<!DOCTYPE movie [<!ENTITY title 'unsafe'>]><movie><title>&title;</title></movie>"
        )
        .is_none()
    );
    let body = format!("<movie><title>{}</title></movie>", "x".repeat(1024 * 1024));
    assert!(parse_nfo_identity(&body).is_none());
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("Movie.2022.strm");
    std::fs::write(&path, "fixture").unwrap();
    std::fs::write(path.with_extension("nfo"), body).unwrap();
    assert!(
        read_watch_identity(&path, temp.path(), MediaKind::Movie)
            .unwrap()
            .is_none()
    );
}

#[test]
fn episode_nfo_can_fill_coordinates_but_not_work_title_year_or_alias() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("video.strm");
    std::fs::write(&path, "fixture").unwrap();
    std::fs::write(path.with_extension("nfo"), "<episodedetails><title>Episode title</title><year>2026</year><season>0</season><episode>1</episode><tmdbid>999</tmdbid></episodedetails>").unwrap();
    std::fs::write(
        temp.path().join("tvshow.nfo"),
        "<tvshow><title>Work</title><year>2022</year><tmdbid>1</tmdbid></tvshow>",
    )
    .unwrap();
    let media = Media {
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
    };
    let outcome = scan_watch(
        &WatchJob {
            kind: WatchKind::InPlace,
            path: temp.path().into(),
            library_root: temp.path().into(),
            tv_library_root: None,
            scrape: false,
        },
        &media,
    )
    .unwrap();
    let parsed = outcome.transferred[0].identified_release.as_ref().unwrap();
    assert_eq!((parsed.season, parsed.episode), (Some(0), Some(1)));
    assert_eq!(parsed.title, "Work");
    assert_eq!(parsed.year, Some(2022));
    assert_eq!(
        outcome.transferred[0]
            .nfo_identity
            .as_ref()
            .unwrap()
            .tmdb_id
            .as_deref(),
        Some("1")
    );
}
