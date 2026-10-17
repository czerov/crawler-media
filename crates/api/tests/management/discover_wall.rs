use std::collections::HashMap;
use std::sync::Arc;

use api::catalog::Catalog;
use api::router;
use axum::http::StatusCode;
use domain::{Media, MediaKind};
use downloader::MemoryDownloader;
use media::CatalogHit;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;

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
        poster_path: Some("https://image.tmdb.org/t/p/w342/matrix.jpg".into()),
        backdrop_path: Some("https://image.tmdb.org/t/p/w1280/matrix-back.jpg".into()),
        rating: Some(8.7),
        overview: Some("A computer hacker learns from mysterious rebels...".into()),
    }
}

fn show() -> CatalogHit {
    CatalogHit {
        media: Media {
            id: domain::MediaId::new(),
            kind: MediaKind::Tv,
            title: "The Long Watch".into(),
            year: Some(2024),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("99999".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        },
        poster_path: Some("https://image.tmdb.org/t/p/w342/long-watch.jpg".into()),
        backdrop_path: None,
        rating: Some(7.5),
        overview: None,
    }
}

struct FailingSectionCatalog;

impl Catalog for FailingSectionCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![movie()])
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![show()])
    }
    fn trending_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Err("upstream connection timed out".into())
    }
    fn now_playing_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Err("rate limit exceeded".into())
    }
    fn details(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
    fn poster_url(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
}

#[tokio::test]
async fn discover_section_failure_degrades_gracefully_without_null_or_502() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(
        state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .with_catalog(Arc::new(FailingSectionCatalog)),
    );

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
    let body = json_body(response).await;
    let raw_text = body.to_string();
    // 敏感内部报错信息绝不向客户端泄露
    assert!(!raw_text.contains("upstream connection timed out"));

    let sections = body["data"]["sections"]
        .as_array()
        .expect("sections is an array");
    assert_eq!(sections.len(), 20);

    for s in sections {
        assert!(!s.is_null(), "no section should be null");
        assert!(s.get("id").is_some(), "every section has an id");
        assert!(s.get("title").is_some(), "every section has a title");
        assert!(s.get("items").is_some(), "every section has an items array");
    }

    let failed_trending = sections
        .iter()
        .find(|s| s["id"] == "featured-weekly")
        .unwrap();
    assert_eq!(failed_trending["items"].as_array().unwrap().len(), 0);
    assert_eq!(failed_trending["error"]["code"], "discover.section_failed");
    assert_eq!(
        failed_trending["error"]["message"],
        "分区暂时无法加载，请重试"
    );

    let popular = sections.iter().find(|s| s["id"] == "popular").unwrap();
    assert_eq!(popular["items"].as_array().unwrap().len(), 1);
    assert!(popular.get("error").is_none());
}

struct EmptyUpcomingCatalog;

impl Catalog for EmptyUpcomingCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![movie()])
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![show()])
    }
    fn trending_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn now_playing_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn upcoming_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
    fn poster_url(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
}

#[tokio::test]
async fn discover_successful_empty_section_has_items_array_without_error() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(
        state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .with_catalog(Arc::new(EmptyUpcomingCatalog)),
    );

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
    let body = json_body(response).await;
    let sections = body["data"]["sections"].as_array().unwrap();

    let upcoming = sections.iter().find(|s| s["id"] == "upcoming").unwrap();
    assert_eq!(upcoming["items"].as_array().unwrap().len(), 0);
    assert!(
        upcoming.get("error").is_none(),
        "successful empty section must have no error field"
    );
    assert_eq!(upcoming["title"], "即将上映");
}
