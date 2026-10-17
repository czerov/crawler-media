//! Regression coverage for nested-library ownership and destructive cleanup authorization.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::{Fixtures, json_body, request, state};

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ))
}

async fn create_member(app: &axum::Router, login: &str) -> (domain::UserId, String) {
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/users",
            Some("management-secret"),
            json!({ "login": login, "password": "member-pass" }),
        ))
        .await
        .unwrap();
    let id =
        domain::UserId::from_str(json_body(created).await["data"]["id"].as_str().unwrap()).unwrap();
    let login = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": login, "password": "member-pass" }),
        ))
        .await
        .unwrap();
    let token = json_body(login).await["data"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    (id, token)
}

fn seed_nested_hidden_movie(store: &Store, child_root: &std::path::Path, member_id: domain::UserId) -> Media {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Secret Movie".into(),
        year: Some(2026),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: media.id,
            path: child_root.join("Secret.Movie.mkv").display().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    store
        .upsert_session(&api::store::SessionRow {
            device_id: "legacy-hidden".into(),
            user_id: member_id,
            media_id: media.id,
            season: None,
            episode: None,
            client: Some("test".into()),
            device_name: Some("test".into()),
            client_version: None,
            play_method: "local".into(),
            position_ms: 10,
            start_position_ms: 0,
            duration_ms: Some(100),
            paused: false,
            watched_ms: 10,
            rate_bps: 0,
            bytes_sent: 0,
            connections: 1,
            admin_ended: false,
            started_at: now,
            last_report_at: now,
            last_report_at_ms: now * 1000,
            ledger_id: None,
            play_session_id: None,
            last_event_sequence: None,
        })
        .unwrap();
    store
        .close_session(member_id, "legacy-hidden", now + 1)
        .unwrap();
    media
}

#[tokio::test]
async fn nested_hidden_library_does_not_leak_through_visible_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (member_id, member) = create_member(&app, "nested-member").await;
    let store = Store::open(tmp.path().join("data")).unwrap();
    let parent = store
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|library| library.kind == MediaKind::Movie && library.is_default)
        .unwrap();
    let child_root = parent.root_paths[0].join("private");
    std::fs::create_dir_all(&child_root).unwrap();
    let child = store
        .create_library(
            MediaKind::Movie,
            "Private",
            &[child_root.to_str().unwrap()],
            "selected",
            true,
            &[],
        )
        .unwrap();
    let media = seed_nested_hidden_movie(&store, &child_root, member_id);
    drop(store);

    let search = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/search/library-items?keyword=Secret",
            Some(&member),
            Value::Null,
        ))
        .await
        .unwrap();
    assert!(
        json_body(search).await["data"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    for uri in [
        format!("/api/v1/libraries/{}/items/{}", parent.id, media.id),
        format!("/api/v1/libraries/{}/items/{}", child.id, media.id),
        format!("/api/v1/playback/items/{}", media.id),
    ] {
        let response = app
            .clone()
            .oneshot(request("GET", &uri, Some(&member), Value::Null))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
    }
    let decide = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/decide",
            Some(&member),
            json!({ "media_item_id": media.id.to_string() }),
        ))
        .await
        .unwrap();
    assert_eq!(decide.status(), StatusCode::NOT_FOUND);

    let history = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/history",
                Some(&member),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(history["data"]["entries"].as_array().unwrap().is_empty());
    assert_eq!(history["data"]["hidden_count"], 1);
    let stats = json_body(
        app.oneshot(request(
            "GET",
            "/api/v1/playback/stats/watch",
            Some(&member),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(stats["data"]["current"]["plays"], 0);
    assert_eq!(stats["data"]["hidden_title_count"], 1);
}

#[tokio::test]
async fn hidden_tv_library_history_does_not_fall_back_to_visible_movie_library() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (member_id, member_token) = create_member(&app, "alice_tv").await;

    // 1. Movie library is default and everyone can see it
    let movie_root = tmp.path().join("movie_root");
    std::fs::create_dir_all(&movie_root).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Visible Movies",
            &[movie_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    // 2. TV library is selected-only and member is NOT in it (hidden)
    let tv_root = tmp.path().join("tv_root");
    std::fs::create_dir_all(&tv_root).unwrap();
    let _tv_lib = store
        .create_library(
            MediaKind::Tv,
            "Hidden TV",
            &[tv_root.to_str().unwrap()],
            "selected",
            true,
            &[], // empty members -> hidden from member
        )
        .unwrap();

    // 3. Insert TV media and ledger, and log a playback for member
    let tv_media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Secret TV Show".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&tv_media).unwrap();
    let tv_file = tv_root.join("show.s01e01.mkv");
    std::fs::write(&tv_file, b"video").unwrap();
    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: tv_media.id,
            path: tv_file.display().to_string(),
            season: Some(1),
            episode: Some(1),
            resolution: Some("1080p".into()),
            codec: Some("H264".into()),
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();

    store
        .upsert_session(&api::store::SessionRow {
            device_id: "dev-tv".into(),
            user_id: member_id,
            media_id: tv_media.id,
            season: Some(1),
            episode: Some(1),
            client: Some("test".into()),
            device_name: Some("test".into()),
            client_version: None,
            play_method: "local".into(),
            position_ms: 50,
            start_position_ms: 0,
            duration_ms: Some(100),
            paused: false,
            watched_ms: 50,
            rate_bps: 0,
            bytes_sent: 0,
            connections: 1,
            admin_ended: false,
            started_at: 1000,
            last_report_at: 1000,
            last_report_at_ms: 1_000_000,
            ledger_id: None,
            play_session_id: None,
            last_event_sequence: None,
        })
        .unwrap();
    store.close_session(member_id, "dev-tv", 1050).unwrap();
    drop(store);

    // 4. Query history as member: must NOT fall back to visible Movie library!
    let history = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/history",
                Some(&member_token),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        history["data"]["entries"].as_array().unwrap().is_empty(),
        "Hidden TV library must not appear in history via movie default fallback: {history:?}"
    );
    assert_eq!(history["data"]["hidden_count"], 1);
}
