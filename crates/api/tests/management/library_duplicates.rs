//! Duplicates: same-unit duplicate grouping + per-file physical deletion.

use std::collections::HashMap;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
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

fn seed_media(
    tmp: &tempfile::TempDir,
) -> (
    axum::Router,
    MediaId,
    String,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let movie = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&movie).unwrap();
    let a = movie.join("The.Matrix.1999.2160p.mkv");
    let b = movie.join("The.Matrix.1999.1080p.mkv");
    std::fs::write(&a, b"aaa").unwrap();
    std::fs::write(&b, b"bbb").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: Some(1999),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    for path in [&a, &b] {
        store
            .insert_ledger(&LedgerRow {
                id: LedgerId::new(),
                media_id: media.id,
                path: path.display().to_string(),
                season: None,
                episode: None,
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
            })
            .unwrap();
    }
    let app = app(tmp);
    (app, media.id, media.id.to_string(), a, b)
}

#[tokio::test]
async fn delete_library_item_removes_physical_files() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _media_id, media_raw, a, b) = seed_media(&tmp);
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
    let library_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    assert!(a.exists());
    assert!(b.exists());

    let deleted = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/libraries/{library_id}/items/{media_raw}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);

    assert!(!a.exists(), "physical file a removed");
    assert!(!b.exists(), "physical file b removed");
}

#[tokio::test]
async fn delete_library_item_keeps_same_media_in_another_library() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media_id, media_raw, a, b) = seed_media(&tmp);
    let other_root = tmp.path().join("data/library/other-movies");
    std::fs::create_dir_all(&other_root).unwrap();
    let other_file = other_root.join("The.Matrix.1999.720p.mkv");
    std::fs::write(&other_file, b"other").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "另一电影库",
            &[other_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id,
            path: other_file.display().to_string(),
            season: None,
            episode: None,
            resolution: Some("720p".into()),
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();
    drop(store);
    let libraries = json_body(
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
    let library_id = libraries["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|library| library["name"] == "电影库")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let response = app
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/libraries/{library_id}/items/{media_raw}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!a.exists());
    assert!(!b.exists());
    assert!(
        other_file.exists(),
        "the other library must keep its own copy"
    );
    let remaining = Store::open(tmp.path().join("data"))
        .unwrap()
        .ledger_for_media(media_id)
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].path, other_file.display().to_string());
}

#[tokio::test]
async fn duplicates_group_and_delete_one() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _media_id, _media_raw, a, b) = seed_media(&tmp);
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
    let library_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let dups = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{library_id}/duplicates"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let groups = dups["data"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "same-media rows group as duplicates");
    assert_eq!(groups[0]["title"], "The Matrix");
    assert_eq!(groups[0]["files"].as_array().unwrap().len(), 2);

    let file_id = groups[0]["files"][0]["file_id"]
        .as_str()
        .unwrap()
        .to_string();
    let deleted = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/libraries/{library_id}/duplicates/{file_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);

    let dups_after = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{library_id}/duplicates"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    // 只剩一个文件 → 不再构成重复组。
    assert!(dups_after["data"].as_array().unwrap().is_empty());
    let kept = if a.exists() { &a } else { &b };
    assert!(kept.exists(), "one physical duplicate remains");
    let remaining = Store::open(tmp.path().join("data"))
        .unwrap()
        .list_ledger()
        .unwrap();
    assert_eq!(remaining.len(), 1, "one ledger row remains");
    let unique_id = remaining[0].id.to_string();
    let unique_delete = app
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/libraries/{library_id}/duplicates/{unique_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(unique_delete.status(), StatusCode::BAD_REQUEST);
    assert!(
        kept.exists(),
        "a unique file must not be deleted by the duplicate endpoint"
    );
}

#[tokio::test]
async fn duplicates_do_not_delete_the_last_present_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _media_id, _media_raw, a, b) = seed_media(&tmp);
    std::fs::remove_file(&b).unwrap();
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
    let library_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let listed = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{library_id}/duplicates"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        listed["data"].as_array().unwrap().is_empty(),
        "缺一份在位文件后不能再当重复组"
    );
    let store = Store::open(tmp.path().join("data")).unwrap();
    let present_id = store
        .list_ledger()
        .unwrap()
        .into_iter()
        .find(|row| row.path == a.display().to_string())
        .unwrap()
        .id;
    let blocked = app
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/libraries/{library_id}/duplicates/{present_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::BAD_REQUEST);
    assert!(a.exists(), "最后一份在位副本绝不能被 duplicates 删除");
}

#[tokio::test]
async fn parent_library_duplicates_cannot_delete_nested_child_library_files() {
    let tmp = tempfile::tempdir().unwrap();
    let parent_root = tmp.path().join("movies");
    let child_root = parent_root.join("private");
    std::fs::create_dir_all(&child_root).unwrap();
    let a = child_root.join("Show.2160p.mkv");
    let b = child_root.join("Show.1080p.mkv");
    std::fs::write(&a, b"aaa").unwrap();
    std::fs::write(&b, b"bbb").unwrap();
    let app = app(&tmp);
    let parent = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "全部电影",
                    "kind": "movie",
                    "root_paths": [parent_root.display().to_string()],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let child = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "私有电影",
                    "kind": "movie",
                    "root_paths": [child_root.display().to_string()],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let store = Store::open(tmp.path().join("data")).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Nested".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    for path in [&a, &b] {
        store
            .insert_ledger(&LedgerRow {
                id: LedgerId::new(),
                media_id: media.id,
                path: path.display().to_string(),
                season: None,
                episode: None,
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
            })
            .unwrap();
    }
    drop(store);
    let parent_id = parent["data"]["id"].as_str().unwrap();
    let child_id = child["data"]["id"].as_str().unwrap();
    let parent_dups = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{parent_id}/duplicates"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        parent_dups["data"].as_array().unwrap().is_empty(),
        "父库不能把子库文件列成自己的重复项"
    );
    let child_dups = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{child_id}/duplicates"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let file_id = child_dups["data"][0]["files"][0]["file_id"]
        .as_str()
        .unwrap();
    let blocked = app
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/libraries/{parent_id}/duplicates/{file_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::NOT_FOUND);
    assert!(a.exists() && b.exists(), "父库 duplicates 绝不能删子库文件");
}
