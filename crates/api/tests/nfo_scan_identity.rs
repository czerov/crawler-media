#[path = "management/common.rs"]
mod common;
use axum::http::StatusCode;
use domain::{Media, MediaId, MediaKind};
use parking_lot::Mutex;
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};
use tower::ServiceExt;

#[derive(Default)]
struct ScanCatalog {
    requests: Mutex<Vec<String>>,
    hit: Option<Media>,
}
impl api::catalog::Catalog for ScanCatalog {
    fn search_movie(&self, query: &str) -> Result<Vec<media::CatalogHit>, String> {
        self.requests.lock().push(format!("search:{query}"));
        Ok(self
            .hit
            .clone()
            .into_iter()
            .map(|media| media::CatalogHit {
                media,
                poster_path: None,
                backdrop_path: None,
                rating: None,
                overview: None,
            })
            .collect())
    }
    fn search_tv(&self, query: &str) -> Result<Vec<media::CatalogHit>, String> {
        self.search_movie(query)
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, _: MediaKind, id: &str) -> Result<Option<Media>, String> {
        self.requests.lock().push(format!("details:{id}"));
        Err("offline fixture".into())
    }
    fn metadata_with_preferences(
        &self,
        _: MediaKind,
        id: &str,
        _: &[String],
        _: &[String],
    ) -> Result<Option<media::ItemMeta>, String> {
        self.requests.lock().push(format!("metadata:{id}"));
        Err("offline fixture".into())
    }
}

#[tokio::test]
async fn public_scan_recovers_existing_media_from_local_work_alias() {
    let temp = tempfile::tempdir().unwrap();
    let state = common::state(
        temp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
    );
    let catalog = Arc::new(ScanCatalog::default());
    let state = state.with_catalog(catalog.clone());
    let shared = state.store();
    let (root, id, old) = {
        let store = shared.lock();
        let library = store
            .list_libraries()
            .unwrap()
            .into_iter()
            .find(|lib| lib.kind == MediaKind::Tv)
            .unwrap();
        let old = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "旧中文标题".into(),
            year: Some(2022),
            original_title: None,
            tmdb_id: Some("207890".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&old).unwrap();
        (library.root_paths[0].clone(), library.id, old)
    };
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("English.Name.2025.S01E03.strm");
    std::fs::write(&path, "https://invalid.example/fixture").unwrap();
    let nfo = "<tvshow><title>新中文标题</title><year>2022</year><tmdbid>207890</tmdbid></tvshow>";
    std::fs::write(root.join("tvshow.nfo"), nfo).unwrap();
    std::fs::write(root.join("poster.jpg"), b"local-poster").unwrap();
    std::fs::write(root.join("fanart.jpg"), b"local-fanart").unwrap();
    let response = api::router(state)
        .oneshot(common::request(
            "POST",
            &format!("/api/v1/libraries/{id}/scan"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let store = shared.lock();
    let row = store
        .ledger_by_path(&path.display().to_string())
        .unwrap()
        .unwrap();
    assert!(
        catalog.requests.lock().is_empty(),
        "complete local NFO must suppress catalog requests"
    );
    assert_eq!(
        std::fs::read(root.join("poster.jpg")).unwrap(),
        b"local-poster"
    );
    assert_eq!(
        std::fs::read(root.join("fanart.jpg")).unwrap(),
        b"local-fanart"
    );
    assert!(store.is_probe_queued(&row.id.to_string()).unwrap());
    assert!(store.get_file_meta(&row.id.to_string()).unwrap().is_none());
    assert_eq!(row.media_id, old.id);
    assert_eq!((row.season, row.episode), (Some(1), Some(3)));
    assert_eq!(
        std::fs::read_to_string(root.join("tvshow.nfo")).unwrap(),
        nfo
    );
}

#[tokio::test]
async fn incomplete_nfo_online_alias_rebinds_only_new_scanned_rows() {
    let temp = tempfile::tempdir().unwrap();
    let state = common::state(
        temp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
    );
    let shared = state.store();
    let (root, id, old, provisional, unrelated) = {
        let store = shared.lock();
        let library = store
            .list_libraries()
            .unwrap()
            .into_iter()
            .find(|lib| lib.kind == MediaKind::Movie)
            .unwrap();
        let old = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "旧中文标题".into(),
            year: Some(2022),
            original_title: None,
            tmdb_id: Some("207890".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        let mut provisional = old.clone();
        provisional.id = MediaId::new();
        provisional.title = "English Name".into();
        provisional.tmdb_id = None;
        store.insert_media(&old).unwrap();
        store.insert_media(&provisional).unwrap();
        let unrelated = domain::LedgerRow {
            id: domain::LedgerId::new(),
            media_id: provisional.id,
            path: temp
                .path()
                .join("unrelated/English.Name.2022.mkv")
                .display()
                .to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Release,
            confidence: domain::Confidence::High,
            filter_score: None,
        };
        store.insert_ledger(&unrelated).unwrap();
        (
            library.root_paths[0].clone(),
            library.id,
            old,
            provisional,
            unrelated,
        )
    };
    let mut hit = old.clone();
    hit.title = "English Name".into();
    let catalog = Arc::new(ScanCatalog {
        hit: Some(hit),
        ..Default::default()
    });
    let state = state.with_catalog(catalog.clone());
    std::fs::create_dir_all(&root).unwrap();
    let paths = [
        root.join("English.Name.2022.1080p.strm"),
        root.join("English.Name.2022.720p.strm"),
    ];
    for path in &paths {
        std::fs::write(path, "https://invalid.example/fixture").unwrap();
        std::fs::write(
            path.with_extension("nfo"),
            "<movie><title>English Name</title></movie>",
        )
        .unwrap();
    }
    let response = api::router(state)
        .oneshot(common::request(
            "POST",
            &format!("/api/v1/libraries/{id}/scan"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        catalog
            .requests
            .lock()
            .iter()
            .any(|request| request.starts_with("search:"))
    );
    let store = shared.lock();
    for path in &paths {
        let row = store
            .ledger_by_path(&path.display().to_string())
            .unwrap()
            .unwrap();
        assert_eq!(row.media_id, old.id);
        assert!(store.is_probe_queued(&row.id.to_string()).unwrap());
    }
    let untouched = store.ledger_by_path(&unrelated.path).unwrap().unwrap();
    assert_eq!(untouched.id, unrelated.id);
    assert_eq!(untouched.media_id, provisional.id);
    assert!(
        store
            .get_media(provisional.id)
            .unwrap()
            .unwrap()
            .tmdb_id
            .is_none()
    );
}
