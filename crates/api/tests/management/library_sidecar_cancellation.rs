//! Frozen Library mutations must fail before filesystem work or catalog IO.
use super::common::*;
use api::{Store, router};
use axum::http::StatusCode;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::json;
use std::{collections::HashMap, sync::Arc};
use tower::ServiceExt;

#[tokio::test]
async fn frozen_library_rejects_organize_reidentify_and_sidecar_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let state = state(
        tmp.path(),
        fetcher,
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    );
    let root = tmp.path().join("extra-movies");
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("original.mkv");
    std::fs::write(&source, b"original-video").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Extra",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Original".into(),
        year: None,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: source.display().to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).unwrap();
    store.begin_library_removal(&library.id).unwrap();
    let app = router(state);
    let destination = root.join("new/renamed.mkv");
    let mutations = [
        (
            format!("/api/v1/libraries/{}/organize", library.id),
            json!({"renames":[{"from":source,"to":destination}]}),
        ),
        (
            "/api/v1/reidentify".into(),
            json!({"file_ids":[row.id.to_string()],"title_ref":"tmdb:movie:123"}),
        ),
        (
            format!(
                "/api/v1/libraries/{}/items/{}/metadata/refresh",
                library.id, media.id
            ),
            json!(null),
        ),
        (
            format!(
                "/api/v1/libraries/{}/items/{}/chapters/generate",
                library.id, media.id
            ),
            json!(null),
        ),
    ];
    for (url, body) in mutations {
        let response = app
            .clone()
            .oneshot(request("POST", &url, Some("management-secret"), body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT, "{url}");
    }
    assert_eq!(std::fs::read(&source).unwrap(), b"original-video");
    assert!(!destination.exists());
    assert!(!root.join("new").exists());
    assert!(!source.with_extension("nfo").exists());
    assert_eq!(
        store
            .get_ledger(&row.id.to_string().replace('-', ""))
            .unwrap()
            .unwrap()
            .path,
        row.path
    );
    assert_eq!(
        store.get_media(media.id).unwrap().unwrap().title,
        "Original"
    );
}
