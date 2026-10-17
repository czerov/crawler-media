use std::collections::HashMap;
use std::fs;
use std::sync::Arc;

use api::{ScrapeStoreExt, Store, router};
use axum::http::StatusCode;
use domain::Confidence;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

#[tokio::test]
async fn unidentified_empty_store_is_empty_list() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/unidentified",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let rows = json_data(listed).await;
    assert_eq!(rows.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn unidentified_lists_parked_intake_path() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let path = tmp.path().join("intake/??.mkv");
    store
        .insert_unidentified(path.display().to_string(), Confidence::Low)
        .unwrap();
    drop(store);
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/unidentified",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let rows = json_data(listed).await;
    let row = &rows.as_array().unwrap()[0];
    assert_eq!(row["path"], path.display().to_string());
    assert_eq!(row["confidence"], "low");
}

#[tokio::test]
async fn claim_unidentified_transfers_into_library() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let intake = tmp.path().join("intake");
    std::fs::create_dir_all(&intake).unwrap();
    let src = intake.join("foo.mkv");
    std::fs::write(&src, b"video").unwrap();
    store
        .insert_unidentified(src.display().to_string(), Confidence::Low)
        .unwrap();
    drop(store);
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));

    let claimed = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/unidentified/claim",
            Some("management-secret"),
            json!({
                "path": src.display().to_string(),
                "title": "The Matrix",
                "kind": "movie",
                "year": 1999
            }),
        ))
        .await
        .unwrap();
    assert_eq!(claimed.status(), StatusCode::OK);

    let listed = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/unidentified",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(json_data(listed).await.as_array().unwrap().len(), 0);

    let ledger = app
        .oneshot(request(
            "GET",
            "/api/v1/ledger",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let rows = json_data(ledger).await;
    let row = &rows.as_array().unwrap()[0];
    assert_eq!(row["media_title"], "The Matrix");
    assert!(std::path::Path::new(row["path"].as_str().unwrap()).is_file());
}

#[tokio::test]
async fn claim_writes_real_ids_and_downloads_poster() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let intake = tmp.path().join("intake");
    std::fs::create_dir_all(&intake).unwrap();
    let src = intake.join("foo.mkv");
    std::fs::write(&src, b"video").unwrap();
    store
        .insert_unidentified(src.display().to_string(), Confidence::Low)
        .unwrap();
    drop(store);
    let app = router(
        state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .with_catalog(Arc::new(super::catalog::PosterCatalog))
        .with_poster_fetch(Arc::new(StaticPoster)),
    );
    app.clone()
        .oneshot(request(
            "PUT",
            "/api/v1/directory",
            Some("management-secret"),
            json!({
                "movie_root": tmp.path().join("movies").display().to_string(),
                "tv_root": tmp.path().join("tv").display().to_string(),
                "transfer_mode": "copy",
                "movie_naming": "{title} ({year})",
                "tv_naming": "{title}/Season {season:02}/{title} - S{season:02}E{episode:02}",
                "scrape": true
            }),
        ))
        .await
        .unwrap();
    let claimed = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/unidentified/claim",
            Some("management-secret"),
            json!({
                "path": src.display().to_string(),
                "title": "The Matrix",
                "kind": "movie",
                "year": 1999,
                "tmdb_id": "603"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(claimed.status(), StatusCode::OK);
    let ledger = app
        .oneshot(request(
            "GET",
            "/api/v1/ledger",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let body = json_data(ledger).await;
    let row = &body.as_array().unwrap()[0];
    let dir = std::path::Path::new(row["path"].as_str().unwrap())
        .parent()
        .unwrap()
        .to_path_buf();
    let nfo_path = fs::read_dir(&dir)
        .unwrap()
        .find_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension().and_then(|e| e.to_str()) == Some("nfo")).then_some(path)
        })
        .unwrap();
    let nfo = fs::read_to_string(nfo_path).unwrap();
    assert!(nfo.contains("<tmdbid>603</tmdbid>"), "{nfo}");
    assert_eq!(fs::read(dir.join("poster.jpg")).unwrap(), b"poster-bytes");
}

#[tokio::test]
async fn claim_rejects_overwriting_existing_ledger_file() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));

    let movie_root = tmp.path().join("movies");
    std::fs::create_dir_all(&movie_root).unwrap();

    let _ = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/directory",
            Some("management-secret"),
            json!({
                "movie_root": movie_root.display().to_string(),
                "tv_root": tmp.path().join("tv").display().to_string(),
                "transfer_mode": "copy",
                "movie_naming": "{title} ({year})",
                "scrape": false
            }),
        ))
        .await
        .unwrap();

    // 准备一个未识别文件，尝试认领
    let unknown_file = tmp.path().join("stage/other.mkv");
    std::fs::create_dir_all(tmp.path().join("stage")).unwrap();
    std::fs::write(&unknown_file, b"new-bytes").unwrap();

    let store = Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(domain::Media {
            id: domain::MediaId::new(),
            kind: domain::MediaKind::Movie,
            title: "The Matrix".into(),
            year: Some(1999),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("603".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    let naming = store.naming_pattern(domain::MediaKind::Movie).unwrap();
    let root = store.library_root(domain::MediaKind::Movie).unwrap();
    let release = domain::Release {
        title: "The Matrix".into(),
        year: Some(1999),
        ..release::parse("other.mkv")
    };
    let existing_path =
        library::render_path(&root, &naming, &media, &release, &unknown_file).unwrap();
    if let Some(parent) = existing_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&existing_path, b"original-bytes").unwrap();

    store
        .insert_ledger(&domain::LedgerRow {
            id: domain::LedgerId::new(),
            media_id: media.id,
            path: existing_path.display().to_string(),
            season: None,
            episode: None,
            resolution: Some("1080p".into()),
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();

    store
        .insert_unidentified(unknown_file.display().to_string(), Confidence::Low)
        .unwrap();
    drop(store);

    let res = app
        .oneshot(request(
            "POST",
            "/api/v1/unidentified/claim",
            Some("management-secret"),
            json!({
                "path": unknown_file.display().to_string(),
                "title": "The Matrix",
                "kind": "movie",
                "year": 1999,
                "tmdb_id": "603"
            }),
        ))
        .await
        .unwrap();

    // 必须被 409 Conflict 拒绝，且原文件字节不被破坏
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(std::fs::read(&existing_path).unwrap(), b"original-bytes");
}

#[tokio::test]
async fn claim_refuses_to_overwrite_an_unledgered_existing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let unknown_file = tmp.path().join("stage/other.mkv");
    std::fs::create_dir_all(unknown_file.parent().unwrap()).unwrap();
    std::fs::write(&unknown_file, b"new-bytes").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(domain::Media {
            id: domain::MediaId::new(),
            kind: domain::MediaKind::Movie,
            title: "The Matrix".into(),
            year: Some(1999),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("603".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let naming = store.naming_pattern(domain::MediaKind::Movie).unwrap();
    let root = store.library_root(domain::MediaKind::Movie).unwrap();
    let release = domain::Release {
        title: "The Matrix".into(),
        year: Some(1999),
        ..release::parse("other.mkv")
    };
    let existing_path =
        library::render_path(&root, &naming, &media, &release, &unknown_file).unwrap();
    std::fs::create_dir_all(existing_path.parent().unwrap()).unwrap();
    std::fs::write(&existing_path, b"original-bytes").unwrap();
    store
        .insert_unidentified(unknown_file.display().to_string(), Confidence::Low)
        .unwrap();
    drop(store);

    let res = app
        .oneshot(request(
            "POST",
            "/api/v1/unidentified/claim",
            Some("management-secret"),
            json!({
                "path": unknown_file.display().to_string(),
                "title": "The Matrix",
                "kind": "movie",
                "year": 1999,
                "tmdb_id": "603"
            }),
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(std::fs::read(&existing_path).unwrap(), b"original-bytes");
    assert_eq!(std::fs::read(&unknown_file).unwrap(), b"new-bytes");
}

pub struct StaticPoster;

impl api::PosterFetch for StaticPoster {
    fn get(&self, _url: &str) -> Result<Vec<u8>, String> {
        Ok(b"poster-bytes".to_vec())
    }
}
