//! Playback views at the public HTTP seam: item poster URL, marks/favorites,
//! up-next, and admin end-session / unsupported revoke.
//!
//! Clock-driven playback behavior (progress heartbeats → sessions/logs/units,
//! activity/devices/end/revoke, stats + history windows, session issue time,
//! marks/metrics timestamps) lives in the sibling `playback_clock` module.

use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
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

/// Seed one movie file in a movie library root and scan it, returning the
/// media id via the ledger.
///
/// Shared with the sibling `playback_clock` module.
pub(super) async fn seed_movie(app: &axum::Router, tmp: &tempfile::TempDir) -> String {
    let movie = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&movie).unwrap();
    std::fs::write(movie.join("The.Matrix.1999.2160p.mkv"), b"matrix").unwrap();
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
    let movie_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    scan_and_wait(&app, &movie_id).await;
    let ledger = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/ledger",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    ledger["data"][0]["media_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn playback_item_uses_ledger_id_for_public_poster_url() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let media_id = seed_movie(&app, &tmp).await;
    let ledger = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/ledger",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let row = &ledger["data"][0];
    let ledger_id = row["id"].as_str().unwrap();
    let parent = std::path::Path::new(row["path"].as_str().unwrap())
        .parent()
        .unwrap();
    std::fs::write(parent.join("poster.jpg"), b"poster").unwrap();

    let item = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/playback/items/{media_id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let expected = format!("/posters/{}", ledger_id);
    assert_eq!(item["data"]["poster_url"], expected);

    let poster = app
        .oneshot(request(
            "GET",
            &format!("/api/v1{expected}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(
        poster.status(),
        StatusCode::OK,
        "visible artwork is authenticated"
    );
}

/// 带管理员 token 的 POST，共享给 sibling `playback_clock` 模块。
pub(super) async fn post_async(
    app: &axum::Router,
    uri: &str,
    body: Value,
) -> axum::response::Response {
    app.clone()
        .oneshot(request("POST", uri, Some("management-secret"), body))
        .await
        .unwrap()
}

/// 带管理员 token 的 GET，返回信封里的 `data`。共享给 `playback_clock`。
pub(super) async fn get_data(app: &axum::Router, uri: &str) -> Value {
    let body = json_body(
        app.clone()
            .oneshot(request("GET", uri, Some("management-secret"), Value::Null))
            .await
            .unwrap(),
    )
    .await;
    body["data"].clone()
}

#[tokio::test]
async fn marks_cascade_and_favorites_list() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let media_id = seed_movie(&app, &tmp).await;

    let marked = post_async(
        &app,
        "/api/v1/playback/marks",
        json!({ "media_item_id": media_id, "played": true, "favorite": true }),
    )
    .await;
    assert_eq!(marked.status(), StatusCode::OK);

    let marks = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/playback/marks?media_item_id={media_id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(marks["data"]["played"], true);
    assert_eq!(marks["data"]["is_favorite"], true);

    // If an episode is favorited, query for whole show must also reflect is_favorite = true
    let tv_id = uuid::Uuid::new_v4().to_string();
    let _ = post_async(
        &app,
        "/api/v1/playback/marks",
        json!({
            "media_item_id": tv_id,
            "season_number": 1,
            "episode_number": 1,
            "favorite": true
        }),
    )
    .await;

    let tv_whole_marks = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/playback/marks?media_item_id={tv_id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(tv_whole_marks["data"]["is_favorite"], true);

    let favorites = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/favorites",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(favorites["data"]["total"], 1);
    assert_eq!(favorites["data"]["items"][0]["title"], "The Matrix");
}

#[tokio::test]
async fn up_next_lists_next_episode_for_tv_subscription() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let tv = tmp.path().join("data/library/tv");
    std::fs::create_dir_all(&tv).unwrap();
    std::fs::write(tv.join("The.Expanse.S01E01.1080p.mkv"), b"expanse").unwrap();
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
    let tv_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "tv")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    scan_and_wait(&app, &tv_id).await;
    let ledger = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/ledger",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let media_id = ledger["data"][0]["media_id"].as_str().unwrap().to_string();

    let subscribe = create_subscribe(&app, "search").await;
    let _ = subscribe;
    let up_next = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/up-next",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    // No subscription matching this media → empty list (valid shape).
    assert!(up_next["data"]["items"].is_array());

    // 当用户播放了该本地扫库剧集（无订阅）后，即使无订阅也必须出现在 up-next
    let progress_res = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({
            "media_item_id": media_id,
            "season": 1,
            "episode": 1,
            "position_ms": 120_000,
            "duration_ms": 2_400_000,
            "device_id": "test-device"
        }),
    )
    .await;
    assert_eq!(progress_res.status(), StatusCode::OK);

    let up_next2 = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/up-next",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let items = up_next2["data"]["items"].as_array().unwrap();
    assert_eq!(
        items.len(),
        1,
        "本地扫库且无订阅的剧集在播放后成功展示在接下来看"
    );
    assert_eq!(items[0]["media_item_id"], media_id);
    assert_eq!(items[0]["season_number"], 1);
    assert_eq!(items[0]["episode_number"], 1);
    assert_eq!(items[0]["position_ms"], 120_000);
}

#[tokio::test]
async fn end_web_session_and_reject_unsupported_revoke() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let media_id = seed_movie(&app, &tmp).await;
    let start = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "start", "device_id": "tv-1" }),
    )
    .await;
    assert_eq!(start.status(), StatusCode::OK);

    let ended = post_async(
        &app,
        "/api/v1/playback/activity/sessions/tv-1/end",
        Value::Null,
    )
    .await;
    assert_eq!(ended.status(), StatusCode::OK);

    let activity = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/activity",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(activity["data"]["sessions"].as_array().unwrap().is_empty());

    // Web has no device credential: unsupported revocation must not block progress.
    let start2 = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "start", "device_id": "blocked-1" }),
    )
    .await;
    assert_eq!(start2.status(), StatusCode::OK);
    let revoke = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/playback/devices/blocked-1",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(revoke.status(), StatusCode::BAD_REQUEST);
    let blocked = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "progress", "device_id": "blocked-1", "position_ms": 10 }),
    )
    .await;
    assert_eq!(blocked.status(), StatusCode::OK);
}
#[path = "playback_subtitles.rs"]
mod subtitles;
