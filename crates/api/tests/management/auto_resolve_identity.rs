use super::*;
#[tokio::test]
async fn http_scan_rebinds_incoming_rows_to_canonical_media_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let old_canonical = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "Canonical Orbit".into(),
        year: Some(2022),
        original_title: None,
        tmdb_id: Some("orbit-tmdb".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    struct OrbitCatalog(media::CatalogHit);
    impl api::catalog::Catalog for OrbitCatalog {
        fn search_movie(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
            Ok(vec![self.0.clone()])
        }
        fn search_tv(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
            Ok(Vec::new())
        }
        fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> { Ok(Vec::new()) }
        fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> { Ok(Vec::new()) }
        fn details(&self, _kind: domain::MediaKind, _id: &str) -> Result<Option<domain::Media>, String> { Ok(None) }
    }
    let hit = media::CatalogHit {
        media: domain::Media {
            id: domain::MediaId::new(),
            kind: domain::MediaKind::Movie,
            title: "Orbit".into(),
            year: Some(2022),
            original_title: None,
            tmdb_id: Some("orbit-tmdb".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        },
        poster_path: None,
        backdrop_path: None,
        rating: None,
        overview: None,
    };
    let catalog = Arc::new(OrbitCatalog(hit));
    let state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .with_catalog(catalog);

    // 预存旧的 canonical Media
    state.store().lock().insert_media(&old_canonical).unwrap();

    let (root, lib_id) = {
        let store_arc = state.store();
        let store = store_arc.lock();
        let lib = store
            .list_libraries()
            .unwrap()
            .into_iter()
            .find(|l| l.kind == domain::MediaKind::Movie)
            .unwrap();
        (lib.root_paths[0].clone(), lib.id)
    };
    std::fs::create_dir_all(&root).unwrap();
    let video = root.join("Orbit.2022.1080p.mkv");
    std::fs::write(&video, b"fake video bytes").unwrap();

    let app = router(state.clone());
    let res = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{lib_id}/scan"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let store_arc = state.store();
    let store = store_arc.lock();
    let row = store
        .ledger_by_path(&video.display().to_string())
        .unwrap()
        .expect("scanned ledger should exist");
    assert_eq!(row.media_id, old_canonical.id, "新增台账必须重新绑定到已有 canonical Media");
}
