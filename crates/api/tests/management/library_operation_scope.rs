use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    router(state(tmp.path(), fetcher, downloader))
}

async fn seed_test_library(app: &axum::Router, name: &str, dir: &std::path::Path) -> String {
    std::fs::create_dir_all(dir).unwrap();
    let res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/libraries",
            Some("management-secret"),
            serde_json::json!({
                "kind": "movie",
                "name": name,
                "root_paths": [dir.to_str().unwrap()],
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    json_body(res).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn seed_parent_and_nested_ledgers(
    tmp: &tempfile::TempDir,
    media_id: domain::MediaId,
    parent_file: &std::path::Path,
    nested_file: &std::path::Path,
) {
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: domain::MediaKind::Movie,
            title: "The Matrix".into(),
            year: Some(1999),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            bangumi_id: None,
            anilist_id: None,
            tvdb_id: None,
        })
        .unwrap();
    store
        .insert_ledger(&domain::LedgerRow {
            id: domain::LedgerId::new(),
            media_id,
            path: parent_file.to_str().unwrap().into(),
            season: None,
            episode: None,
            resolution: Some("1080p".into()),
            codec: Some("H264".into()),
            hdr: None,
            quality_source: domain::QualitySource::Probe,
            confidence: domain::Confidence::High,
            filter_score: None,
        })
        .unwrap();
    store
        .insert_ledger(&domain::LedgerRow {
            id: domain::LedgerId::new(),
            media_id,
            path: nested_file.to_str().unwrap().into(),
            season: None,
            episode: None,
            resolution: Some("2160p".into()),
            codec: Some("HEVC".into()),
            hdr: None,
            quality_source: domain::QualitySource::Probe,
            confidence: domain::Confidence::High,
            filter_score: None,
        })
        .unwrap();
}

#[tokio::test]
async fn deleting_item_in_parent_library_does_not_delete_nested_library_file() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let parent_dir = tmp.path().join("media");
    let nested_dir = parent_dir.join("private");

    let parent_id = seed_test_library(&app, "Parent Library A", &parent_dir).await;
    let _nested_id = seed_test_library(&app, "Nested Library B", &nested_dir).await;

    // 2. 种入同一个 MediaId 在父库和嵌套子库的两个文件
    let media_id = domain::MediaId::new();
    let parent_file = parent_dir.join("The.Matrix.1999.mkv");
    let nested_file = nested_dir.join("The.Matrix.1999.Private.mkv");
    std::fs::write(&parent_file, b"parent-movie").unwrap();
    std::fs::write(&nested_file, b"nested-movie").unwrap();
    seed_parent_and_nested_ledgers(&tmp, media_id, &parent_file, &nested_file);

    // 3. 在父库 A 中删除该 media_id 条目
    let del_res = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/libraries/{parent_id}/items/{media_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(del_res.status(), StatusCode::OK);

    // 4. 关键验证：父库的文件被删除了，但嵌套子库 B 的文件和台账绝对不能被删除！
    assert!(!parent_file.exists(), "父库文件应被删除");
    assert!(
        nested_file.exists(),
        "嵌套子库的文件绝对不能被父库越权删除！"
    );

    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let remaining_ledger = store.ledger_for_media(media_id).unwrap();
    assert_eq!(remaining_ledger.len(), 1, "嵌套子库的台账必须保留");
    assert_eq!(remaining_ledger[0].path, nested_file.to_str().unwrap());
}
