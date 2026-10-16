use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use domain::MediaKind;
use media::{CatalogGet, Tmdb, TmdbError};

struct FakeTmdb {
    bodies: HashMap<String, String>,
    hits: Arc<Mutex<Vec<String>>>,
}

impl CatalogGet for FakeTmdb {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        self.hits.lock().unwrap().push(path.to_string());
        // The real client appends language=zh-CN to the request; fixtures are
        // keyed by the base query, so drop just that segment.
        let key = path
            .replace("&language=zh-CN", "")
            .replace("?language=zh-CN", "");
        self.bodies
            .get(&key)
            .cloned()
            .ok_or_else(|| TmdbError::Http(format!("missing {key}")))
    }
}

fn bodies() -> HashMap<String, String> {
    HashMap::from([
        (
            "/search/movie?query=matrix".into(),
            include_str!("fixtures/search_movie.json").into(),
        ),
        (
            "/search/tv?query=breaking".into(),
            include_str!("fixtures/search_tv.json").into(),
        ),
        (
            "/movie/popular".into(),
            include_str!("fixtures/popular_movie.json").into(),
        ),
        (
            "/movie/603".into(),
            include_str!("fixtures/movie_details.json").into(),
        ),
        (
            "/tv/1396".into(),
            include_str!("fixtures/tv_details.json").into(),
        ),
    ])
}

fn client(dir: &std::path::Path) -> Tmdb<FakeTmdb> {
    Tmdb::new(
        FakeTmdb {
            bodies: bodies(),
            hits: Arc::new(Mutex::new(Vec::new())),
        },
        &dir.join("catalog.db"),
    )
    .unwrap()
}

#[test]
fn search_movie_returns_pickable_media() {
    let dir = tempfile::tempdir().unwrap();
    let tmdb = client(dir.path());
    let hits = tmdb.search_movie("matrix").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].media.tmdb_id.as_deref(), Some("603"));
    assert_eq!(hits[0].media.title, "The Matrix");
    assert_eq!(hits[0].media.kind, MediaKind::Movie);
    assert!(hits[0].media.douban_id.is_none());
    assert_eq!(
        hits[0].poster_path.as_deref(),
        Some("/f89U3ADr1oiB1s9GkdPOEpXUk5H.jpg")
    );
}

#[test]
fn popular_movie_returns_pickable_media() {
    let dir = tempfile::tempdir().unwrap();
    let tmdb = client(dir.path());
    let hits = tmdb.popular_movie().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].media.title, "The Matrix");
    assert_eq!(
        hits[0].poster_path.as_deref(),
        Some("/f89U3ADr1oiB1s9GkdPOEpXUk5H.jpg")
    );
}

#[test]
fn search_tv_returns_pickable_media() {
    let dir = tempfile::tempdir().unwrap();
    let tmdb = client(dir.path());
    let hits = tmdb.search_tv("breaking").unwrap();
    assert_eq!(hits[0].media.tmdb_id.as_deref(), Some("1396"));
    assert_eq!(hits[0].media.kind, MediaKind::Tv);
    assert_eq!(
        hits[0].poster_path.as_deref(),
        Some("/ggFHVNu6YYI5L9pCfOacjizRGt.jpg")
    );
}

#[test]
fn movie_details_uses_tmdb_id() {
    let dir = tempfile::tempdir().unwrap();
    let tmdb = client(dir.path());
    let media = tmdb.movie_details("603").unwrap();
    assert_eq!(media.title, "The Matrix");
    assert_eq!(media.tmdb_id.as_deref(), Some("603"));
}

#[test]
fn tv_details_uses_tmdb_id() {
    let dir = tempfile::tempdir().unwrap();
    let tmdb = client(dir.path());
    let media = tmdb.tv_details("1396").unwrap();
    assert_eq!(media.title, "Breaking Bad");
    assert_eq!(media.kind, MediaKind::Tv);
}

#[test]
fn movie_details_poster_uses_cached_body() {
    let dir = tempfile::tempdir().unwrap();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let tmdb = Tmdb::new(
        FakeTmdb {
            bodies: bodies(),
            hits: hits.clone(),
        },
        &dir.path().join("catalog.db"),
    )
    .unwrap();
    let poster = tmdb.details_poster(MediaKind::Movie, "603").unwrap();
    assert_eq!(poster.as_deref(), Some("/f89U3ADr1oiB1s9GkdPOEpXUk5H.jpg"));
    tmdb.details_poster(MediaKind::Movie, "603").unwrap();
    assert_eq!(hits.lock().unwrap().len(), 1);
}

#[test]
fn cache_hit_skips_second_http() {
    let dir = tempfile::tempdir().unwrap();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let tmdb = Tmdb::new(
        FakeTmdb {
            bodies: bodies(),
            hits: hits.clone(),
        },
        &dir.path().join("catalog.db"),
    )
    .unwrap();
    tmdb.search_movie("matrix").unwrap();
    tmdb.search_movie("matrix").unwrap();
    assert_eq!(hits.lock().unwrap().len(), 1);
    assert!(!dir.path().join("tmdb").exists());
    assert!(dir.path().join("catalog.db").is_file());
}

#[test]
fn missing_path_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let tmdb = Tmdb::new(
        FakeTmdb {
            bodies: HashMap::new(),
            hits: Arc::new(Mutex::new(Vec::new())),
        },
        &dir.path().join("catalog.db"),
    )
    .unwrap();
    assert!(tmdb.search_movie("nope").is_err());
}

#[test]
fn expired_row_refetches() {
    // 固定起点时钟（0 表示「实时时钟」，测试必须用非 0 值钉住时间）。
    const NOW: i64 = 1_000_000;
    let dir = tempfile::tempdir().unwrap();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let tmdb = Tmdb::new_at(
        FakeTmdb {
            bodies: bodies(),
            hits: hits.clone(),
        },
        &dir.path().join("catalog.db"),
        NOW,
    )
    .unwrap();
    tmdb.search_movie("matrix").unwrap();
    tmdb.set_now(NOW + media::DEFAULT_TTL_SECS + 1);
    tmdb.search_movie("matrix").unwrap();
    assert_eq!(hits.lock().unwrap().len(), 2);
}

#[test]
fn stale_row_is_returned_when_http_fails() {
    const NOW: i64 = 1_000_000;
    let dir = tempfile::tempdir().unwrap();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let tmdb = Tmdb::new_at(
        FakeTmdb {
            bodies: bodies(),
            hits: hits.clone(),
        },
        &dir.path().join("catalog.db"),
        NOW,
    )
    .unwrap();
    tmdb.search_movie("matrix").unwrap();
    tmdb.set_now(NOW + media::DEFAULT_TTL_SECS + 1);
    // empty bodies → HTTP error, stale must still parse
    let tmdb = Tmdb::new_at(
        FakeTmdb {
            bodies: HashMap::new(),
            hits: hits.clone(),
        },
        &dir.path().join("catalog.db"),
        NOW + media::DEFAULT_TTL_SECS + 1,
    )
    .unwrap();
    let hits_count = tmdb.search_movie("matrix").unwrap();
    assert_eq!(hits_count[0].media.title, "The Matrix");
}
#[test]
fn cached_season_details_reads_fresh_stale_and_miss_without_fetching() {
    const NOW: i64 = 1_000_000;
    let dir = tempfile::tempdir().unwrap();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let tmdb = Tmdb::new_at(FakeTmdb {
        bodies: HashMap::from([(
            "/tv/1396/season/1".to_string(),
            include_str!("fixtures/rich_season_details.json").to_string(),
        )]),
        hits: hits.clone(),
    }, &dir.path().join("catalog.db"), NOW).unwrap();
    assert!(tmdb.cached_season_details("1396", 1).unwrap().is_none());
    tmdb.season_details("1396", 1).unwrap();
    let fresh = tmdb.cached_season_details("1396", 1).unwrap().unwrap();
    assert!(!fresh.is_empty());
    tmdb.set_now(NOW + media::DEFAULT_TTL_SECS + 1);
    let stale = tmdb.cached_season_details("1396", 1).unwrap().unwrap();
    assert_eq!(stale, fresh);
    assert_eq!(hits.lock().unwrap().len(), 1, "cache-only reads must never fetch");
}

#[test]
fn tmdb_new_at_returns_err_when_cache_cannot_open() {
    let blocked = tempfile::tempdir().unwrap();
    let file = blocked.path().join("not-a-dir");
    std::fs::write(&file, b"x").unwrap();
    let result = Tmdb::new_at(
        FakeTmdb {
            bodies: HashMap::new(),
            hits: Arc::new(Mutex::new(Vec::new())),
        },
        &file.join("catalog.db"),
        1,
    );
    assert!(result.is_err());
}

#[test]
fn tv_external_ids_return_the_tvdb_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut map = bodies();
    map.insert(
        "/tv/119847/external_ids".into(),
        r#"{"id":119847,"tvdb_id":425039,"imdb_id":"tt13351446"}"#.into(),
    );
    let tmdb = Tmdb::new(
        FakeTmdb {
            bodies: map,
            hits: Arc::new(Mutex::new(Vec::new())),
        },
        &dir.path().join("catalog.db"),
    )
    .unwrap();
    let tvdb_id = tmdb.tvdb_id("119847").unwrap();
    assert_eq!(tvdb_id.as_deref(), Some("425039"));
}
