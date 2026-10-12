//! `GET /libraries/{id}/items` 与 `/gallery` 的显式分页校验。
//!
//! 越界的显式 `limit`/`offset` 必须 400，不再静默 clamp；`offset + limit` 不得
//! 溢出；**省略 `limit` 仍返回全部**（默认分页迁移是独立决策，本任务不动）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use api::ApiState;
use api::router;
use axum::http::StatusCode;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

#[path = "management/common.rs"]
mod common;
use common::{Fixtures, json_body, request, state};

fn base_state(tmp: &tempfile::TempDir) -> ApiState {
    state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
}

fn movie_library(state: &ApiState) -> (String, PathBuf) {
    let library = state
        .store()
        .lock()
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|library| library.kind == MediaKind::Movie)
        .expect("default movie library");
    (library.id, library.root_paths[0].clone())
}

fn seed_movie(state: &ApiState, title: &str, root: &Path, with_poster: bool) {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.into(),
        year: Some(2024),
        original_title: Some(title.into()),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    state.store().lock().insert_media(&media).unwrap();
    let dir = root.join(title);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{title}.mkv"));
    std::fs::write(&file, b"video").unwrap();
    if with_poster {
        std::fs::write(dir.join("poster.jpg"), b"poster").unwrap();
    }
    state
        .store()
        .lock()
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: media.id,
            path: file.to_string_lossy().to_string(),
            season: None,
            episode: None,
            resolution: Some("1080p".into()),
            codec: Some("H264".into()),
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();
}

async fn get(app: &axum::Router, url: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(request("GET", url, Some("management-secret"), Value::Null))
        .await
        .unwrap();
    let status = response.status();
    (status, json_body(response).await)
}

fn fixture() -> (tempfile::TempDir, axum::Router, String) {
    let tmp = tempfile::tempdir().unwrap();
    let api_state = base_state(&tmp);
    let (library_id, root) = movie_library(&api_state);
    for index in 0..5 {
        // 只有前两个带海报，用来验证 gallery 的"先剔除空图组再分页"。
        seed_movie(&api_state, &format!("Movie {index}"), &root, index < 2);
    }
    (tmp, router(api_state), library_id)
}

#[tokio::test]
async fn invalid_explicit_paging_is_rejected_on_both_paths() {
    let (_tmp, app, library_id) = fixture();
    for bad in ["limit=0", "limit=201", "limit=-1", "limit=abc", "limit="] {
        let (status, body) = get(&app, &format!("/api/v1/libraries/{library_id}/items?{bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "items {bad}: {body}");
        assert_eq!(body["error"]["code"], "library.invalid");
        let (status, body) =
            get(&app, &format!("/api/v1/libraries/{library_id}/gallery?{bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "gallery {bad}: {body}");
    }
    for bad in ["offset=-1", "offset=abc", "offset="] {
        let (status, _) = get(&app, &format!("/api/v1/libraries/{library_id}/items?{bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "items {bad}");
        let (status, _) = get(&app, &format!("/api/v1/libraries/{library_id}/gallery?{bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "gallery {bad}");
    }
}

#[tokio::test]
async fn huge_offset_does_not_overflow_or_panic() {
    let (_tmp, app, library_id) = fixture();
    let offset = usize::MAX;
    for path in ["items", "gallery"] {
        let (status, body) = get(
            &app,
            &format!("/api/v1/libraries/{library_id}/{path}?offset={offset}&limit=10"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        assert_eq!(
            body["data"].as_array().unwrap().len(),
            0,
            "{path} 的 offset 超出长度应返回空列表"
        );
    }
}

#[tokio::test]
async fn explicit_limit_returns_the_requested_page() {
    let (_tmp, app, library_id) = fixture();
    let (status, body) = get(
        &app,
        &format!("/api/v1/libraries/{library_id}/items?limit=200"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"].as_array().unwrap().len(), 5);

    let (status, body) = get(&app, &format!("/api/v1/libraries/{library_id}/items?limit=1")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"].as_array().unwrap().len(), 1);

    let (status, body) = get(
        &app,
        &format!("/api/v1/libraries/{library_id}/items?offset=2&limit=2"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn omitted_limit_still_returns_everything() {
    let (_tmp, app, library_id) = fixture();
    let (status, body) = get(&app, &format!("/api/v1/libraries/{library_id}/items")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["data"].as_array().unwrap().len(),
        5,
        "省略 limit 的既有行为是返回全部"
    );

    // gallery：5 个 Media 里只有 2 个有图，空图组先被剔除，再分页。
    let (status, body) = get(&app, &format!("/api/v1/libraries/{library_id}/gallery")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"].as_array().unwrap().len(), 2);

    let (status, body) = get(
        &app,
        &format!("/api/v1/libraries/{library_id}/gallery?offset=1&limit=5"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["data"].as_array().unwrap().len(),
        1,
        "gallery 的分页发生在剔除空图组之后"
    );
}
