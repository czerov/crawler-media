//! B6：`GET /playback/stats/watch` 改为 Store SQL 聚合后，HTTP DTO 与统计语义不变。

#[path = "management/common.rs"]
mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, UserId};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

const DAY: i64 = 86_400;

fn app(tmp: &tempfile::TempDir) -> (axum::Router, ApiState) {
    let state = common::state(
        tmp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    );
    (router(state.clone()), state)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn admin_id(state: &ApiState) -> UserId {
    let store = state.store();
    let store = store.lock();
    store
        .list_users()
        .unwrap()
        .into_iter()
        .find(|user| store.user_role(user.id).ok().as_deref() == Some("admin"))
        .expect("seeded admin")
        .id
}

fn media(title: &str) -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.into(),
        year: Some(1999),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn ledger_row(media_id: MediaId, path: &Path) -> LedgerRow {
    LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: path.display().to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    }
}

/// 直接写 subscribe.db：HTTP 层没有公开的 insert_log。
fn insert_log(
    root: &Path,
    user: UserId,
    media: MediaId,
    started: i64,
    watched: i64,
    client: Option<&str>,
    completed: bool,
) {
    let conn = rusqlite::Connection::open(root.join("data/subscribe.db")).unwrap();
    conn.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    conn.execute(
        "INSERT INTO playback_logs
            (id, user_id, media_id, client, started_at, ended_at, watched_ms, completed)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            user.to_string(),
            media.to_string(),
            client,
            started,
            started + 1,
            watched,
            completed as i64,
        ],
    )
    .unwrap();
}

struct Fixture {
    media_a: Media,
    media_b: Media,
    media_hidden: Media,
    user: UserId,
    now: i64,
}

/// A/B 有可见台账；hidden 没有台账（对 visible scope 不可见）。
fn seed(state: &ApiState, tmp: &tempfile::TempDir) -> Fixture {
    let user = admin_id(state);
    let media_a = media("Alpha");
    let media_b = media("Beta");
    let media_hidden = media("Hidden");
    let root = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&root).unwrap();
    let path_a = root.join("Alpha.mkv");
    let path_b = root.join("Beta.mkv");
    std::fs::write(&path_a, b"a").unwrap();
    std::fs::write(&path_b, b"b").unwrap();
    {
        let store = state.store();
        let store = store.lock();
        for item in [&media_a, &media_b, &media_hidden] {
            store.insert_media(item).unwrap();
        }
        store.insert_ledger(&ledger_row(media_a.id, &path_a)).unwrap();
        store.insert_ledger(&ledger_row(media_b.id, &path_b)).unwrap();
    }
    let now = unix_now();
    Fixture {
        media_a,
        media_b,
        media_hidden,
        user,
        now,
    }
}

#[tokio::test]
async fn watch_stats_dto_matches_existing_semantics() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, state) = app(&tmp);
    let f = seed(&state, &tmp);

    insert_log(&tmp.path(), f.user, f.media_a.id, f.now - 60, 1_000, Some("web"), true);
    insert_log(&tmp.path(), f.user, f.media_a.id, f.now - 120, 2_000, Some("web"), false);
    insert_log(&tmp.path(), f.user, f.media_b.id, f.now - 180, 4_000, None, true);
    // 上一窗口 [now-14d, now-7d)。
    insert_log(&tmp.path(), f.user, f.media_a.id, f.now - 8 * DAY, 5_000, Some("web"), true);
    // 当前窗口内没有台账的 Media → hidden。
    insert_log(&tmp.path(), f.user, f.media_hidden.id, f.now - 240, 8_000, None, false);

    let stats = common::json_data(
        app.oneshot(common::request(
            "GET",
            "/api/v1/playback/stats/watch?days=7&tz_offset=0",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;

    assert_eq!(stats["days"], 7);
    assert_eq!(stats["current"]["plays"], 3);
    assert_eq!(stats["current"]["watched_ms"], 7_000);
    assert_eq!(stats["current"]["completed"], 2);
    assert_eq!(stats["current"]["active_members"], 1);
    assert_eq!(stats["previous"]["plays"], 1);
    assert_eq!(stats["previous"]["watched_ms"], 5_000);
    assert_eq!(stats["previous"]["completed"], 1);
    assert_eq!(stats["previous"]["active_members"], 1);
    assert_eq!(stats["previous_available"], true);
    assert_eq!(stats["hidden_title_count"], 1, "没有台账的 Media 对 visible scope 不可见");

    let by_day = stats["by_day"].as_array().unwrap();
    assert_eq!(by_day.len(), 7);
    assert_eq!(
        by_day.iter().map(|day| day["plays"].as_i64().unwrap()).sum::<i64>(),
        3
    );
    let previous_by_day = stats["previous_by_day"].as_array().unwrap();
    assert_eq!(previous_by_day.len(), 7);
    assert_eq!(
        previous_by_day.iter().map(|day| day["plays"].as_i64().unwrap()).sum::<i64>(),
        1
    );

    let by_hour = stats["by_hour"].as_array().unwrap();
    assert_eq!(by_hour.len(), 7);
    assert!(by_hour.iter().all(|row| row.as_array().unwrap().len() == 24));
    assert_eq!(
        by_hour
            .iter()
            .flat_map(|row| row.as_array().unwrap())
            .map(|cell| cell.as_i64().unwrap())
            .sum::<i64>(),
        7_000
    );

    let by_member = stats["by_member"].as_array().unwrap();
    assert_eq!(by_member.len(), 1);
    assert_eq!(by_member[0]["member_id"], f.user.to_string());
    assert_eq!(by_member[0]["plays"], 3);
    assert_eq!(by_member[0]["watched_ms"], 7_000);
    assert_eq!(by_member[0]["completed"], 2);

    let by_client = stats["by_client"].as_array().unwrap();
    assert_eq!(by_client.len(), 2);
    let web = by_client.iter().find(|row| row["client"] == "web").unwrap();
    assert_eq!(web["plays"], 2);
    assert_eq!(web["watched_ms"], 3_000);
    let direct = by_client.iter().find(|row| row["client"] == "直连").unwrap();
    assert_eq!(direct["plays"], 1);
    assert_eq!(direct["watched_ms"], 4_000);

    assert_eq!(stats["by_tier"], json!([{ "tier": 0, "label": "直连", "plays": 3 }]));

    let top = stats["top_titles"].as_array().unwrap();
    assert_eq!(top.len(), 2);
    assert_eq!(top[0]["media"]["media_item_id"], f.media_b.id.to_string(), "watched 降序");
    assert_eq!(top[0]["watched_ms"], 4_000);
    assert_eq!(top[0]["members"], 1);
    assert_eq!(top[1]["media"]["media_item_id"], f.media_a.id.to_string());

    let favorites = stats["favorites"].as_array().unwrap();
    assert_eq!(favorites.len(), 2);
    assert_eq!(favorites[0]["media"]["media_item_id"], f.media_b.id.to_string());

    let previous_favorites = stats["previous_favorites"].as_array().unwrap();
    assert_eq!(previous_favorites.len(), 1);
    assert_eq!(
        previous_favorites[0]["media"]["media_item_id"],
        f.media_a.id.to_string()
    );
}

#[tokio::test]
async fn scope_all_includes_every_media_and_hides_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, state) = app(&tmp);
    let f = seed(&state, &tmp);
    insert_log(&tmp.path(), f.user, f.media_a.id, f.now - 60, 1_000, Some("web"), true);
    insert_log(&tmp.path(), f.user, f.media_hidden.id, f.now - 120, 8_000, None, false);

    let stats = common::json_data(
        app.oneshot(common::request(
            "GET",
            "/api/v1/playback/stats/watch?days=7&tz_offset=0&scope=all",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;

    assert_eq!(stats["current"]["plays"], 2);
    assert_eq!(stats["current"]["watched_ms"], 9_000);
    assert_eq!(stats["hidden_title_count"], 0, "scope=all 不隐藏");
    assert_eq!(stats["top_titles"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn out_of_range_tz_offset_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _state) = app(&tmp);
    let response = app
        .oneshot(common::request(
            "GET",
            "/api/v1/playback/stats/watch?days=7&tz_offset=2000",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error = common::json_error(response).await;
    assert_eq!(error["code"], "playback.invalid");
}
