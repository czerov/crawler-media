use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use api::catalog::Catalog;
use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{Media, MediaKind};
use downloader::MemoryDownloader;
use media::CatalogHit;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

pub(crate) struct FakeCatalog {
    movies: Vec<CatalogHit>,
    shows: Vec<CatalogHit>,
}

impl Catalog for FakeCatalog {
    fn source_name(&self) -> &'static str {
        "tmdb"
    }
    fn source(&self, name: &str) -> Option<&dyn Catalog> {
        (name == "tmdb").then_some(self)
    }
    fn search_movie(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(self.movies.clone())
    }
    fn search_tv(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(self.shows.clone())
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(self.movies.clone())
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(self.shows.clone())
    }
    fn top_rated_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![hit("movie", "Shawshank", "278")])
    }
    fn top_rated_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![hit("tv", "Planet Earth", "123")])
    }
    fn now_playing_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![hit("movie", "Now Playing Film", "999")])
    }
    fn on_the_air_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![hit("tv", "On The Air Show", "888")])
    }
    fn trending_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![hit("movie", "Trending Film", "777")])
    }
    fn trending_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![hit("tv", "Trending Show", "666")])
    }
    fn upcoming_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![hit("movie", "Upcoming Film", "555")])
    }
    fn airing_today_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![hit("tv", "Airing Today", "444")])
    }
    fn filtered_movie(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![hit("movie", "Filtered Film", "333")])
    }
    fn filtered_tv(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![hit("tv", "Filtered Show", "222")])
    }
    fn details(&self, kind: MediaKind, tmdb_id: &str) -> Result<Option<Media>, String> {
        let hits = match kind {
            MediaKind::Movie | MediaKind::Video => &self.movies,
            MediaKind::Tv => &self.shows,
        };
        Ok(hits
            .iter()
            .find(|hit| hit.media.tmdb_id.as_deref() == Some(tmdb_id))
            .map(|hit| hit.media.clone()))
    }
}

pub(super) fn hit(kind: &str, title: &str, tmdb: &str) -> CatalogHit {
    let media_kind = if kind == "tv" {
        MediaKind::Tv
    } else {
        MediaKind::Movie
    };
    CatalogHit {
        media: Media {
            id: domain::MediaId::new(),
            kind: media_kind,
            title: title.into(),
            year: Some(2024),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some(tmdb.into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        },
        poster_path: Some("/poster.jpg".into()),
        backdrop_path: None,
        rating: None,
        overview: None,
    }
}

fn movie() -> CatalogHit {
    CatalogHit {
        media: Media {
            id: domain::MediaId::new(),
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
        },
        poster_path: Some("/f89U3ADr1oiB1s9GkdPOEpXUk5H.jpg".into()),
        backdrop_path: None,
        rating: None,
        overview: None,
    }
}

fn show() -> CatalogHit {
    CatalogHit {
        media: Media {
            id: domain::MediaId::new(),
            kind: MediaKind::Tv,
            title: "Breaking Bad".into(),
            year: Some(2008),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("1396".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        },
        poster_path: None,
        backdrop_path: None,
        rating: None,
        overview: None,
    }
}

fn douban_movie() -> CatalogHit {
    CatalogHit {
        media: Media {
            id: domain::MediaId::new(),
            kind: MediaKind::Movie,
            title: "黑客帝国".into(),
            year: Some(1999),
            original_title: Some("The Matrix".into()),
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: Some("1291843".into()),
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        },
        poster_path: Some(
            "https://img2.doubanio.com/view/photo/s_ratio_poster/public/p513344864.jpg".into(),
        ),
        backdrop_path: None,
        rating: None,
        overview: None,
    }
}

fn tvdb_movie() -> CatalogHit {
    CatalogHit {
        media: Media {
            id: domain::MediaId::new(),
            kind: MediaKind::Movie,
            title: "The Matrix".into(),
            year: Some(1999),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: Some("169".into()),
            bangumi_id: None,
            anilist_id: None,
        },
        poster_path: Some("https://artworks.thetvdb.com/banners/movies/169/posters/5f8.jpg".into()),
        backdrop_path: None,
        rating: None,
        overview: None,
    }
}

fn bangumi_movie() -> CatalogHit {
    CatalogHit {
        media: Media {
            id: domain::MediaId::new(),
            kind: MediaKind::Movie,
            title: "黑客帝国".into(),
            year: Some(1999),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: Some("1040".into()),
            anilist_id: None,
        },
        poster_path: Some("https://lain.bgm.tv/pic/cover/l/matrix.jpg".into()),
        backdrop_path: None,
        rating: None,
        overview: None,
    }
}

fn anilist_movie() -> CatalogHit {
    CatalogHit {
        media: Media {
            id: domain::MediaId::new(),
            kind: MediaKind::Movie,
            title: "The Matrix".into(),
            year: Some(1999),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: Some("437".into()),
        },
        poster_path: Some(
            "https://s4.anilist.co/file/anilistcdn/media/anime/cover/medium/matrix.jpg".into(),
        ),
        backdrop_path: None,
        rating: None,
        overview: None,
    }
}

pub(crate) struct PosterCatalog;

impl Catalog for PosterCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
    fn poster_url(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Option<String>, String> {
        Ok(Some("https://image.tmdb.org/t/p/w342/p.jpg".into()))
    }
}
pub(crate) fn test_catalog() -> FakeCatalog {
    FakeCatalog {
        movies: vec![movie()],
        shows: vec![show()],
    }
}

fn catalog_state(root: &std::path::Path) -> ApiState {
    state(
        root,
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
    )
    .with_catalog(Arc::new(FakeCatalog {
        movies: vec![movie()],
        shows: vec![show()],
    }))
}

#[tokio::test]
async fn catalog_search_returns_movie_and_tv_media() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(catalog_state(tmp.path()));
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/search/titles?keyword=matrix",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = json_data(response).await;
    let hits = data["titles"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    let movie = hits.iter().find(|row| row["kind"] == "movie").unwrap();
    assert_eq!(movie["title"], "The Matrix");
    assert_eq!(movie["year"], 1999);
    assert_eq!(movie["external_id"], "603");
    assert_eq!(movie["provider"], "tmdb");
    let tv = hits.iter().find(|row| row["kind"] == "tv").unwrap();
    assert_eq!(tv["external_id"], "1396");
    assert_eq!(tv["provider"], "tmdb");
}

#[tokio::test]
async fn catalog_search_includes_douban_only_hit() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(
        catalog_state(tmp.path()).with_catalog(Arc::new(FakeCatalog {
            movies: vec![movie(), douban_movie()],
            shows: vec![show()],
        })),
    );
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/search/titles?keyword=matrix",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = json_data(response).await;
    let hits = data["titles"].as_array().unwrap();
    let douban = hits
        .iter()
        .find(|row| row["external_id"] == "1291843")
        .unwrap();
    assert_eq!(douban["title"], "黑客帝国");
}

#[tokio::test]
async fn subscribe_from_douban_hit_persists_douban_id() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let app = router(catalog_state(tmp.path()));
    let filter = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/rule-sets",
                Some("management-secret"),
                json!({
                    "name": "preferred",
                    "atoms": [{ "kind": "resolution", "value": "1080p", "priority": 50 }]
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    app.clone()
        .oneshot(request(
            "PUT",
            "/api/v1/rule-sets/default",
            Some("management-secret"),
            json!({ "id": filter["id"] }),
        ))
        .await
        .unwrap();
    let created = app
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "media": { "kind": "movie", "title": "黑客帝国", "douban_id": "1291843" },
                "coverage": { "kind": "movie" },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_data(created).await;
    let store = Store::open(&data).unwrap();
    let media_id = domain::MediaId::from_str(created["media"]["id"].as_str().unwrap()).unwrap();
    let media = store.get_media(media_id).unwrap().unwrap();
    assert_eq!(media.douban_id.as_deref(), Some("1291843"));
    assert!(media.tmdb_id.is_none());
}

#[tokio::test]
async fn catalog_search_includes_tvdb_only_hit() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(
        catalog_state(tmp.path()).with_catalog(Arc::new(FakeCatalog {
            movies: vec![movie(), tvdb_movie()],
            shows: vec![show()],
        })),
    );
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/search/titles?keyword=matrix",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = json_data(response).await;
    let hits = data["titles"].as_array().unwrap();
    let tvdb = hits.iter().find(|row| row["external_id"] == "169").unwrap();
    assert_eq!(tvdb["title"], "The Matrix");
}

#[tokio::test]
async fn subscribe_from_tvdb_hit_persists_tvdb_id() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let app = router(catalog_state(tmp.path()));
    let filter = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/rule-sets",
                Some("management-secret"),
                json!({
                    "name": "preferred",
                    "atoms": [{ "kind": "resolution", "value": "1080p", "priority": 50 }]
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    app.clone()
        .oneshot(request(
            "PUT",
            "/api/v1/rule-sets/default",
            Some("management-secret"),
            json!({ "id": filter["id"] }),
        ))
        .await
        .unwrap();
    let created = app
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "media": { "kind": "movie", "title": "The Matrix", "tvdb_id": "169" },
                "coverage": { "kind": "movie" },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_data(created).await;
    let store = Store::open(&data).unwrap();
    let media_id = domain::MediaId::from_str(created["media"]["id"].as_str().unwrap()).unwrap();
    let media = store.get_media(media_id).unwrap().unwrap();
    assert_eq!(media.tvdb_id.as_deref(), Some("169"));
    assert!(media.tmdb_id.is_none());
}

#[tokio::test]
async fn catalog_search_includes_bangumi_only_hit() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(
        catalog_state(tmp.path()).with_catalog(Arc::new(FakeCatalog {
            movies: vec![movie(), bangumi_movie()],
            shows: vec![show()],
        })),
    );
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/search/titles?keyword=matrix",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = json_data(response).await;
    let hits = data["titles"].as_array().unwrap();
    let bangumi = hits
        .iter()
        .find(|row| row["external_id"] == "1040")
        .unwrap();
    assert_eq!(bangumi["title"], "黑客帝国");
}

#[tokio::test]
async fn subscribe_from_bangumi_hit_persists_bangumi_id() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let app = router(catalog_state(tmp.path()));
    let filter = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/rule-sets",
                Some("management-secret"),
                json!({
                    "name": "preferred",
                    "atoms": [{ "kind": "resolution", "value": "1080p", "priority": 50 }]
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    app.clone()
        .oneshot(request(
            "PUT",
            "/api/v1/rule-sets/default",
            Some("management-secret"),
            json!({ "id": filter["id"] }),
        ))
        .await
        .unwrap();
    let created = app
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "media": { "kind": "movie", "title": "黑客帝国", "bangumi_id": "1040" },
                "coverage": { "kind": "movie" },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_data(created).await;
    let store = Store::open(&data).unwrap();
    let media_id = domain::MediaId::from_str(created["media"]["id"].as_str().unwrap()).unwrap();
    let media = store.get_media(media_id).unwrap().unwrap();
    assert_eq!(media.bangumi_id.as_deref(), Some("1040"));
    assert!(media.tmdb_id.is_none());
}

#[tokio::test]
async fn catalog_search_includes_anilist_only_hit() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(
        catalog_state(tmp.path()).with_catalog(Arc::new(FakeCatalog {
            movies: vec![movie(), anilist_movie()],
            shows: vec![show()],
        })),
    );
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/search/titles?keyword=matrix",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = json_data(response).await;
    let hits = data["titles"].as_array().unwrap();
    let anilist = hits.iter().find(|row| row["external_id"] == "437").unwrap();
    assert_eq!(anilist["title"], "The Matrix");
}

#[tokio::test]
async fn subscribe_from_anilist_hit_persists_anilist_id() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let app = router(catalog_state(tmp.path()));
    let filter = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/rule-sets",
                Some("management-secret"),
                json!({
                    "name": "preferred",
                    "atoms": [{ "kind": "resolution", "value": "1080p", "priority": 50 }]
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    app.clone()
        .oneshot(request(
            "PUT",
            "/api/v1/rule-sets/default",
            Some("management-secret"),
            json!({ "id": filter["id"] }),
        ))
        .await
        .unwrap();
    let created = app
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "media": { "kind": "movie", "title": "The Matrix", "anilist_id": "437" },
                "coverage": { "kind": "movie" },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_data(created).await;
    let store = Store::open(&data).unwrap();
    let media_id = domain::MediaId::from_str(created["media"]["id"].as_str().unwrap()).unwrap();
    let media = store.get_media(media_id).unwrap().unwrap();
    assert_eq!(media.anilist_id.as_deref(), Some("437"));
    assert!(media.tmdb_id.is_none());
}

#[tokio::test]
async fn catalog_discover_returns_popular_movies() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(catalog_state(tmp.path()));
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/discover/movie",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = json_data(response).await;
    assert_eq!(data["kind"], "movie");
    let secs = data["sections"].as_array().unwrap();
    assert!(!secs.is_empty());
}

#[tokio::test]
async fn catalog_discover_returns_popular_tv_with_letter_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(catalog_state(tmp.path()));
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/discover/tv",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = json_data(response).await;
    assert_eq!(data["kind"], "tv");
    let secs = data["sections"].as_array().unwrap();
    assert!(!secs.is_empty());
}

#[tokio::test]
async fn subscribe_from_catalog_hit_reuses_tmdb_media_and_default_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(catalog_state(tmp.path()));
    let filter = json_data(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/rule-sets",
                Some("management-secret"),
                json!({
                    "name": "preferred",
                    "atoms": [{ "kind": "resolution", "value": "1080p", "priority": 50 }]
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    app.clone()
        .oneshot(request(
            "PUT",
            "/api/v1/rule-sets/default",
            Some("management-secret"),
            json!({ "id": filter["id"] }),
        ))
        .await
        .unwrap();

    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "media": { "kind": "tv", "title": "Breaking Bad", "tmdb_id": "1396" },
                "coverage": { "kind": "tv", "season": 1, "episode_from": 1 },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_data(created).await;
    assert_eq!(created["filter_id"], filter["id"]);

    let tv_movie_mismatch = app
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "media": { "kind": "tv", "title": "Breaking Bad", "tmdb_id": "1396" },
                "coverage": { "kind": "movie" },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(tv_movie_mismatch.status(), StatusCode::BAD_REQUEST);
}
