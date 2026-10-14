//! Playback activity: progress heartbeats → sessions/logs/units, activity
//! snapshot, history, stats, marks, up-next, favorites, devices.

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

struct FakePlaybackClock(std::sync::atomic::AtomicI64);

impl api::playback_events::PlaybackClock for FakePlaybackClock {
    fn now_ms(&self) -> i64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

fn app_with_clock(tmp: &tempfile::TempDir, clock: Arc<FakePlaybackClock>) -> axum::Router {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    router(state(tmp.path(), fetcher, downloader).with_playback_clock(clock))
}

/// Seed one movie file in a movie library root and scan it, returning the
/// media id via the ledger.
async fn seed_movie(app: &axum::Router, tmp: &tempfile::TempDir) -> String {
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

async fn post_async(app: &axum::Router, uri: &str, body: Value) -> axum::response::Response {
    app.clone()
        .oneshot(request("POST", uri, Some("management-secret"), body))
        .await
        .unwrap()
}

#[tokio::test]
async fn progress_lifecycle_writes_session_log_and_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let now = api::job_loop::unix_now();
    let clock = Arc::new(FakePlaybackClock(std::sync::atomic::AtomicI64::new(now * 1000)));
    let app = app_with_clock(&tmp, clock.clone());
    let media_id = seed_movie(&app, &tmp).await;

    let start = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "start", "device_id": "web-1", "position_ms": 0 }),
    )
    .await;
    assert_eq!(start.status(), StatusCode::OK);
    let body = json_body(start).await;
    assert_eq!(body["data"]["played"], false);
    assert_eq!(body["data"]["position_ms"], 0);

    // Send progress in realistic heartbeat increments (<= 30s)
    for step in 1..=6 {
        clock.0.fetch_add(20_000, std::sync::atomic::Ordering::SeqCst);
        let beat = post_async(
            &app,
            "/api/v1/playback/progress",
            json!({ "media_item_id": media_id, "event": "progress", "device_id": "web-1", "position_ms": step * 20_000, "paused": false }),
        )
        .await;
        assert_eq!(beat.status(), StatusCode::OK);
        if step == 6 {
            let body = json_body(beat).await;
            assert_eq!(body["data"]["position_ms"], 120_000);
        }
    }

    // Active session shows in activity.
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
    let sessions = activity["data"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["device_id"], "web-1");
    assert_eq!(
        sessions[0]["revocable"], false,
        "web player has no credentials"
    );
    assert_eq!(sessions[0]["media"]["title"], "The Matrix");

    // Advance to 300_000 ms in realistic intervals
    for step in 7..=14 {
        clock.0.fetch_add(20_000, std::sync::atomic::Ordering::SeqCst);
        let beat = post_async(
            &app,
            "/api/v1/playback/progress",
            json!({ "media_item_id": media_id, "event": "progress", "device_id": "web-1", "position_ms": step * 20_000, "paused": false }),
        )
        .await;
        assert_eq!(beat.status(), StatusCode::OK);
    }

    // Stop closes the session into a play log.
    clock.0.fetch_add(20_000, std::sync::atomic::Ordering::SeqCst);
    let stop = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "stop", "device_id": "web-1", "position_ms": 300_000 }),
    )
    .await;
    assert_eq!(stop.status(), StatusCode::OK);

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

    let history = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/history",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let entries = history["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["media"]["title"], "The Matrix");
    assert_eq!(entries[0]["watched_ms"], 120_000 + 180_000);
    assert_eq!(entries[0]["completed"], false);

    // Resume reads the unit.
    let resume = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/playback/resume?media_item_id={media_id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(resume["data"]["position_ms"], 300_000);
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

#[tokio::test]
async fn stats_aggregate_and_policy_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let now = api::job_loop::unix_now();
    let clock = Arc::new(FakePlaybackClock(std::sync::atomic::AtomicI64::new(now * 1000)));
    let app = app_with_clock(&tmp, clock.clone());
    let media_id = seed_movie(&app, &tmp).await;
    let start = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "start", "device_id": "web-1", "position_ms": 0 }),
    )
    .await;
    assert_eq!(start.status(), StatusCode::OK);
    for step in 1..=2 {
        clock.0.fetch_add(20_000, std::sync::atomic::Ordering::SeqCst);
        let beat = post_async(
            &app,
            "/api/v1/playback/progress",
            json!({ "media_item_id": media_id, "event": "progress", "device_id": "web-1", "position_ms": step * 20_000, "paused": false }),
        )
        .await;
        assert_eq!(beat.status(), StatusCode::OK);
    }
    clock.0.fetch_add(20_000, std::sync::atomic::Ordering::SeqCst);
    let stop = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "stop", "device_id": "web-1", "position_ms": 60_000 }),
    )
    .await;
    assert_eq!(stop.status(), StatusCode::OK);

    let conn = rusqlite::Connection::open(tmp.path().join("data/subscribe.db")).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    conn.execute(
        "INSERT INTO playback_logs (id, user_id, media_id, started_at, ended_at, watched_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            "00000000-0000-0000-0000-000000000001",
            media_id,
            now - 8 * 86_400,
            now - 8 * 86_400 + 60,
            30_000
        ],
    )
    .unwrap();

    let stats = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/stats/watch?days=7",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(stats["data"]["current"]["plays"], 1);
    assert!(stats["data"]["current"]["watched_ms"].as_i64().unwrap() >= 60_000);
    assert_eq!(stats["data"]["by_day"].as_array().unwrap().len(), 7);
    assert_eq!(stats["data"]["by_hour"].as_array().unwrap().len(), 7);
    assert_eq!(stats["data"]["by_hour"][0].as_array().unwrap().len(), 24);
    assert!(
        stats["data"]["by_day"]
            .as_array()
            .unwrap()
            .iter()
            .any(|day| day["members"] == 1)
    );
    assert_eq!(
        stats["data"]["by_member"][0]["member_id"],
        "00000000-0000-0000-0000-000000000001"
    );
    assert!(
        stats["data"]["top_titles"][0]["watched_ms"]
            .as_i64()
            .unwrap()
            >= 60_000
    );
    assert_eq!(stats["data"]["top_titles"][0]["members"], 1);
    assert_eq!(stats["data"]["previous"]["active_members"], 1);
    assert_eq!(
        stats["data"]["previous_by_day"].as_array().unwrap().len(),
        7
    );
    assert!(
        stats["data"]["previous_by_day"]
            .as_array()
            .unwrap()
            .iter()
            .any(|day| day["plays"] == 1)
    );

    let policy = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/playback/policy",
            Some("management-secret"),
            json!({ "trickplay_enabled": true }),
        ))
        .await
        .unwrap();
    assert_eq!(policy.status(), StatusCode::OK);
    let policy_get = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/policy",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(policy_get["data"]["trickplay_enabled"], true);
    assert_eq!(policy_get["data"]["hardware_available"], false);
}

#[path = "playback_subtitles.rs"]
mod subtitles;

/// 注入的 `PlaybackClock` 必须按毫秒落库：心跳路径不得把 `last_report_at_ms`
/// 量化回整秒（B3 复核：`now * 1000` 会丢掉亚秒精度）。
#[tokio::test]
async fn heartbeat_persists_sub_second_report_time() {
    let tmp = tempfile::tempdir().unwrap();
    let base_ms = 1_700_000_000_000i64;
    let clock = Arc::new(FakePlaybackClock(std::sync::atomic::AtomicI64::new(base_ms)));
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let st = state(tmp.path(), fetcher, downloader);
    let app = router(st.clone().with_playback_clock(clock.clone()));
    let media_id = seed_movie(&app, &tmp).await;

    let start = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "start", "device_id": "web-ms", "position_ms": 0 }),
    )
    .await;
    assert_eq!(start.status(), StatusCode::OK);

    // 只推进 500ms：若实现按秒量化，这个差值会丢失。
    clock.0.fetch_add(500, std::sync::atomic::Ordering::SeqCst);
    let beat = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "progress", "device_id": "web-ms", "position_ms": 0, "paused": false }),
    )
    .await;
    assert_eq!(beat.status(), StatusCode::OK);

    let store = st.store();
    let user = {
        let guard = store.lock();
        guard
            .user_by_token("management-secret")
            .unwrap()
            .unwrap()
            .id
    };
    let session = store
        .lock()
        .get_session(user, "web-ms")
        .unwrap()
        .unwrap();
    assert_eq!(
        session.last_report_at_ms,
        base_ms + 500,
        "心跳必须保留注入时钟的亚秒精度"
    );
}
