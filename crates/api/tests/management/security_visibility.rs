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
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Secret Movie".into(),
        year: Some(2026),
        original_title: None,
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
            ledger_id: None,
        })
        .unwrap();
    store
        .close_session(member_id, "legacy-hidden", now + 1)
        .unwrap();
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
