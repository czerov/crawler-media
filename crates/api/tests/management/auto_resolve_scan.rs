//! Test auto-resolving TMDB metadata during library scan.

use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;

struct WrongYearCatalog;

struct StillCatalog;
struct StillBytes;

impl api::PosterFetch for StillBytes {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        Ok(if url.contains("/still-1.jpg") {
            b"still-bytes".to_vec()
        } else if url.contains("/ep1.jpg") {
            b"one".to_vec()
        } else {
            b"two".to_vec()
        })
    }
}

impl api::catalog::Catalog for StillCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(
        &self,
        _kind: domain::MediaKind,
        _id: &str,
    ) -> Result<Option<domain::Media>, String> {
        Ok(None)
    }
    fn episode_stills(&self, _id: &str, _season: u32, episode: u32) -> Result<Vec<String>, String> {
        Ok(vec![format!("/ep{episode}.jpg")])
    }
}

impl api::catalog::Catalog for WrongYearCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![media::CatalogHit {
            media: domain::Media {
                id: domain::MediaId::new(),
                kind: domain::MediaKind::Movie,
                title: "Arrival".into(),
                year: Some(2016),
                original_title: None,
                tmdb_id: Some("329865".into()),
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            },
            poster_path: None,
            backdrop_path: None,
            rating: None,
            overview: None,
        }])
    }
    fn search_tv(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(
        &self,
        _kind: domain::MediaKind,
        _id: &str,
    ) -> Result<Option<domain::Media>, String> {
        Ok(None)
    }
}

struct AliasYearCatalog {
    queries: Mutex<Vec<String>>,
}

impl api::catalog::Catalog for AliasYearCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, query: &str) -> Result<Vec<media::CatalogHit>, String> {
        self.search_tv_year(query, None)
    }
    fn search_tv_year(
        &self,
        query: &str,
        year: Option<u16>,
    ) -> Result<Vec<media::CatalogHit>, String> {
        self.queries.lock().push(format!(
            "{query}|{}",
            year.map(|y| y.to_string()).unwrap_or_default()
        ));
        if query == "动灵守护者" && year == Some(2022) {
            return Ok(vec![media::CatalogHit {
                media: domain::Media {
                    id: domain::MediaId::new(),
                    kind: domain::MediaKind::Tv,
                    title: "动灵守护者".into(),
                    year: Some(2022),
                    original_title: Some("Spirit Rangers".into()),
                    tmdb_id: Some("207890".into()),
                    douban_id: None,
                    tvdb_id: None,
                    bangumi_id: None,
                    anilist_id: None,
                },
                poster_path: None,
                backdrop_path: None,
                rating: None,
                overview: None,
            }]);
        }
        Ok(vec![media::CatalogHit {
            media: domain::Media {
                id: domain::MediaId::new(),
                kind: domain::MediaKind::Tv,
                title: "Spirit Rangers: The Movie".into(),
                year: Some(2024),
                original_title: None,
                tmdb_id: Some("999".into()),
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            },
            poster_path: None,
            backdrop_path: None,
            rating: None,
            overview: None,
        }])
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(
        &self,
        _kind: domain::MediaKind,
        _id: &str,
    ) -> Result<Option<domain::Media>, String> {
        Ok(None)
    }
    fn metadata_with_preferences(
        &self,
        _kind: domain::MediaKind,
        _id: &str,
        _lang_pref: &[String],
        _country_pref: &[String],
    ) -> Result<Option<media::ItemMeta>, String> {
        Ok(Some(media::ItemMeta {
            overview: Some("Spirit Rangers overview".into()),
            rating: Some("8.5".into()),
            runtime_minutes: Some("25".into()),
            tagline: None,
            release_date: Some("2022-10-10".into()),
            last_air_date: None,
            content_rating: None,
            vote_count: Some(100),
            origin_countries: vec!["US".into()],
            original_language: Some("en".into()),
            studios: vec!["Netflix".into()],
            status: Some("Ended".into()),
            number_of_seasons: Some(1),
            number_of_episodes: Some(10),
            directors: Vec::new(),
            creators: Vec::new(),
            genres: vec!["Animation".into()],
            genre_ids: vec![16],
            episode_run_time: vec![25],
            cast: Vec::new(),
        }))
    }
    fn season_details(&self, _id: &str, _season: u32) -> Result<Vec<media::EpisodeMeta>, String> {
        Ok(vec![media::EpisodeMeta {
            episode_number: 1,
            name: Some("Thunder Mountain".into()),
            overview: Some("Episode 1 overview".into()),
            air_date: Some("2022-10-10".into()),
            rating: Some("8.0".into()),
            vote_count: Some(50),
            runtime_minutes: Some(25),
            still_path: Some("/still-1.jpg".into()),
        }])
    }
    fn tv_seasons(&self, _id: &str) -> Result<Vec<media::TvSeason>, String> {
        Ok(vec![media::TvSeason {
            season_number: 1,
            name: "第 1 季".into(),
            episode_count: Some(1),
            air_date: Some("2022-10-10".into()),
            overview: Some("第一季简介".into()),
            poster_path: Some("/season-1.jpg".into()),
        }])
    }
}

#[test]
fn auto_resolve_uses_ancestor_alias_when_filename_title_misses() {
    let tmp = tempfile::tempdir().unwrap();
    let catalog = Arc::new(AliasYearCatalog {
        queries: Mutex::new(Vec::new()),
    });
    let state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .with_catalog(catalog.clone())
    .with_poster_fetch(Arc::new(StillBytes));
    let root = state
        .store()
        .lock()
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|library| library.kind == domain::MediaKind::Tv)
        .unwrap()
        .root_paths[0]
        .clone();
    let show = root.join("动灵守护者 (2022)/Spirit.Rangers.S01");
    std::fs::create_dir_all(&show).unwrap();
    let episode = show.join("Spirit.Rangers.S01E01.1080p.strm");
    std::fs::write(&episode, b"https://cdn.example/ep.mkv").unwrap();
    let wanted = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Tv,
        title: "Spirit Rangers".into(),
        year: Some(2022),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    store.insert_media(&wanted).unwrap();
    let actual = api::auto_resolve::auto_resolve_media(&state, &wanted, &episode).unwrap();
    assert_eq!(actual.tmdb_id.as_deref(), Some("207890"));
    assert_eq!(actual.year, Some(2022));
    let queries = catalog.queries.lock().clone();
    assert!(queries.iter().any(|query| query == "Spirit Rangers|2022"));
    assert!(queries.iter().any(|query| query == "动灵守护者|2022"));

    // Verify auto_resolve writes tvshow.nfo and episode nfo
    let tvshow_nfo = root.join("动灵守护者 (2022)/tvshow.nfo");
    assert!(
        tvshow_nfo.is_file(),
        "auto_resolve should write tvshow.nfo at show root"
    );
    let episode_nfo = show.join("Spirit.Rangers.S01E01.1080p.nfo");
    assert!(
        episode_nfo.is_file(),
        "auto_resolve should write episode nfo beside video"
    );

    let show_root = root.join("动灵守护者 (2022)");
    let season_nfo = show_root.join("season.nfo");
    let season_body = std::fs::read_to_string(&season_nfo).expect("season.nfo");
    assert!(season_body.contains("<seasonnumber>1</seasonnumber>"), "{season_body}");
    assert!(season_body.contains("第一季简介"), "{season_body}");
    assert!(
        season_body.contains("https://image.tmdb.org/t/p/w780/season-1.jpg"),
        "{season_body}"
    );
    let still = show.join("Spirit.Rangers.S01E01.1080p-thumb.jpg");
    assert_eq!(std::fs::read(&still).unwrap(), b"still-bytes");
}

#[test]
fn auto_resolve_writes_season_nfo_without_thumb_when_still_missing() {
    let tmp = tempfile::tempdir().unwrap();
    struct NoStillCatalog;
    impl api::catalog::Catalog for NoStillCatalog {
        fn search_movie(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
            Ok(Vec::new())
        }
        fn search_tv(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
            Ok(Vec::new())
        }
        fn search_tv_year(&self, _query: &str, _year: Option<u16>) -> Result<Vec<media::CatalogHit>, String> {
            Ok(vec![media::CatalogHit {
                media: domain::Media {
                    id: domain::MediaId::new(),
                    kind: domain::MediaKind::Tv,
                    title: "Spirit Rangers".into(),
                    year: Some(2022),
                    original_title: Some("Spirit Rangers".into()),
                    tmdb_id: Some("207890".into()),
                    douban_id: None,
                    tvdb_id: None,
                    bangumi_id: None,
                    anilist_id: None,
                },
                poster_path: None,
                backdrop_path: None,
                rating: None,
                overview: None,
            }])
        }
        fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
            Ok(Vec::new())
        }
        fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
            Ok(Vec::new())
        }
        fn top_rated_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
            Ok(Vec::new())
        }
        fn top_rated_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
            Ok(Vec::new())
        }
        fn now_playing_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
            Ok(Vec::new())
        }
        fn details(&self, _kind: domain::MediaKind, _id: &str) -> Result<Option<domain::Media>, String> {
            Ok(None)
        }
        fn metadata_with_preferences(
            &self,
            _kind: domain::MediaKind,
            _id: &str,
            _lang_pref: &[String],
            _country_pref: &[String],
        ) -> Result<Option<media::ItemMeta>, String> {
            Ok(Some(media::ItemMeta {
                overview: Some("Spirit Rangers overview".into()),
                rating: Some("8.5".into()),
                runtime_minutes: Some("25".into()),
                tagline: None,
                release_date: Some("2022-10-10".into()),
                last_air_date: None,
                content_rating: None,
                vote_count: Some(100),
                origin_countries: vec!["US".into()],
                original_language: Some("en".into()),
                studios: vec!["Netflix".into()],
                status: Some("Ended".into()),
                number_of_seasons: Some(1),
                number_of_episodes: Some(10),
                directors: Vec::new(),
                creators: Vec::new(),
                genres: vec!["Animation".into()],
                genre_ids: vec![16],
                episode_run_time: vec![25],
                cast: Vec::new(),
            }))
        }
        fn season_details(&self, _id: &str, _season: u32) -> Result<Vec<media::EpisodeMeta>, String> {
            Ok(vec![media::EpisodeMeta {
                episode_number: 1,
                name: Some("Thunder Mountain".into()),
                overview: Some("Episode 1 overview".into()),
                air_date: Some("2022-10-10".into()),
                rating: Some("8.0".into()),
                vote_count: Some(50),
                runtime_minutes: Some(25),
                still_path: None,
            }])
        }
        fn tv_seasons(&self, _id: &str) -> Result<Vec<media::TvSeason>, String> {
            Ok(vec![media::TvSeason {
                season_number: 1,
                name: "第 1 季".into(),
                episode_count: Some(1),
                air_date: Some("2022-10-10".into()),
                overview: Some("第一季简介".into()),
                poster_path: Some("/season-1.jpg".into()),
            }])
        }
    }

    let state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .with_catalog(Arc::new(NoStillCatalog))
    .with_poster_fetch(Arc::new(StillBytes));

    let show = tmp
        .path()
        .join("children/动灵守护者 (2022)/Spirit.Rangers.S01");
    std::fs::create_dir_all(&show).unwrap();
    let episode = show.join("Spirit.Rangers.S01E01.1080p.strm");
    std::fs::write(&episode, b"https://cdn.example/ep.mkv").unwrap();
    let wanted = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Tv,
        title: "Spirit Rangers".into(),
        year: Some(2022),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    store.insert_media(&wanted).unwrap();
    let actual = api::auto_resolve::auto_resolve_media(&state, &wanted, &episode).unwrap();
    assert_eq!(actual.tmdb_id.as_deref(), Some("207890"));

    let show_root = tmp.path().join("children/动灵守护者 (2022)");
    let season_nfo = show_root.join("season.nfo");
    assert!(season_nfo.is_file(), "season.nfo should exist");
    let still = show.join("Spirit.Rangers.S01E01.1080p-thumb.jpg");
    assert!(!still.exists(), "thumb should not exist when still_path is None");
}

#[test]
fn auto_resolve_rejects_catalog_hit_for_different_year() {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .with_catalog(Arc::new(WrongYearCatalog));
    let wanted = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "Arrival".into(),
        year: Some(1996),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    store.insert_media(&wanted).unwrap();
    let actual = api::auto_resolve::auto_resolve_media(
        &state,
        &wanted,
        &tmp.path().join("Arrival.1996.mkv"),
    );
    assert!(actual.is_none());
    assert!(
        store
            .get_media(wanted.id)
            .unwrap()
            .unwrap()
            .tmdb_id
            .is_none()
    );
}

fn app(tmp: &tempfile::TempDir) -> (axum::Router, api::Store) {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let st = state(tmp.path(), fetcher, downloader);
    (router(st), store)
}

#[tokio::test]
async fn library_scan_auto_resolves_tmdb_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, store) = app(&tmp);

    let tv_dir = tmp.path().join("data/library/tv/Frieren");
    std::fs::create_dir_all(&tv_dir).unwrap();

    let strm_file = tv_dir.join("葬送的芙莉莲 - S01E01.strm");
    std::fs::write(&strm_file, "https://cdn.example.com/ep01.mkv\n").unwrap();

    // Query libraries to get TV library id
    let libs = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/libraries",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let tv_lib_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "tv")
        .unwrap()["id"]
        .as_str()
        .unwrap();

    // Trigger scan
    let scan_res = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{tv_lib_id}/scan"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(scan_res.status(), StatusCode::OK);

    // Verify media was auto-recorded in ledger
    let ledger = store.list_ledger().unwrap();
    assert_eq!(ledger.len(), 1);
    let row = &ledger[0];
    assert_eq!(row.season, Some(1));
    assert_eq!(row.episode, Some(1));

    let media = store.get_media(row.media_id).unwrap().unwrap();
    assert_eq!(media.title, "葬送的芙莉莲");
}

#[tokio::test]
async fn scan_keeps_same_title_different_years_and_kinds_separate() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, store) = app(&tmp);
    let movie_dir = tmp.path().join("data/library/movies");
    let tv_dir = tmp.path().join("data/library/tv");
    std::fs::create_dir_all(&movie_dir).unwrap();
    std::fs::create_dir_all(&tv_dir).unwrap();
    let paths = [
        movie_dir.join("Arrival.1996.1080p.mkv"),
        movie_dir.join("Arrival.2016.1080p.mkv"),
        tv_dir.join("Arrival.2016.S01E01.mkv"),
    ];
    for path in &paths {
        std::fs::write(path, b"video").unwrap();
    }
    let libs = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/libraries",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    for kind in ["movie", "tv"] {
        let id = libs["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["kind"] == kind)
            .unwrap()["id"]
            .as_str()
            .unwrap();
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/libraries/{id}/scan"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let rows = store.list_ledger().unwrap();
    assert_eq!(rows.len(), 3);
    let media: Vec<_> = rows
        .iter()
        .map(|row| store.get_media(row.media_id).unwrap().unwrap())
        .collect();
    assert_eq!(
        media
            .iter()
            .map(|m| m.id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );
    assert!(
        media
            .iter()
            .any(|m| m.year == Some(1996) && m.kind == domain::MediaKind::Movie)
    );
    assert!(
        media
            .iter()
            .any(|m| m.year == Some(2016) && m.kind == domain::MediaKind::Movie)
    );
    assert!(
        media
            .iter()
            .any(|m| m.year == Some(2016) && m.kind == domain::MediaKind::Tv)
    );
}

#[tokio::test]
async fn metadata_refresh_writes_each_episode_still_in_shared_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let st = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .with_catalog(Arc::new(StillCatalog))
    .with_poster_fetch(Arc::new(StillBytes));
    let app = router(st);
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let dir = tmp.path().join("data/library/tv");
    std::fs::create_dir_all(&dir).unwrap();
    for episode in 1..=2 {
        std::fs::write(
            dir.join(format!("Orbit.S01E{episode:02}.strm")),
            "https://example.com/video.mkv",
        )
        .unwrap();
    }
    let libs = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/libraries",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "tv")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let scan = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{id}/scan"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(scan.status(), StatusCode::OK);
    let rows = store.list_ledger().unwrap();
    assert_eq!(rows.len(), 2);
    let mut item = store.get_media(rows[0].media_id).unwrap().unwrap();
    item.tmdb_id = Some("123".into());
    store.update_media(&item).unwrap();
    let refresh = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{id}/metadata/refresh"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(refresh.status(), StatusCode::OK);
    assert_eq!(
        std::fs::read(dir.join("Orbit.S01E01-thumb.jpg")).unwrap(),
        b"one"
    );
    assert_eq!(
        std::fs::read(dir.join("Orbit.S01E02-thumb.jpg")).unwrap(),
        b"two"
    );
}

#[path = "auto_resolve_identity.rs"]
mod identity;
