//! Regression coverage for member-owned playback sessions and saved UI/search state.

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

fn seed_movie(tmp: &tempfile::TempDir, users: &[domain::UserId]) -> MediaId {
    let store = Store::open(tmp.path().join("data")).unwrap();
    let library = store
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|library| library.kind == MediaKind::Movie && library.is_default)
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Shared Movie".into(),
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
            path: library.root_paths[0]
                .join("Shared.Movie.mkv")
                .display()
                .to_string(),
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
    for user in users {
        store
            .upsert_unit(
                *user,
                media.id,
                -1,
                -1,
                0,
                None,
                None,
                Some(7_200_000),
                None,
                None,
                false,
                1,
            )
            .unwrap();
    }
    media.id
}

async fn send_playback(
    app: &axum::Router,
    token: &str,
    media_id: MediaId,
    event: &str,
) -> axum::response::Response {
    app.clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/progress",
            Some(token),
            json!({"media_item_id":media_id.to_string(),"event":event,"device_id":"shared-device"}),
        ))
        .await
        .unwrap()
}

#[tokio::test]
async fn same_web_device_id_is_scoped_per_user_and_revoke_is_unsupported() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (alice_id, alice) = create_member(&app, "device-alice").await;
    let (bob_id, bob) = create_member(&app, "device-bob").await;
    let media_id = seed_movie(&tmp, &[alice_id, bob_id]);
    for (id, token) in [(alice_id, &alice), (bob_id, &bob)] {
        assert_eq!(
            send_playback(&app, token, media_id, "start").await.status(),
            StatusCode::OK
        );
        let activity = app
            .clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/activity",
                Some(token),
                Value::Null,
            ))
            .await
            .unwrap();
        let body = json_body(activity).await;
        assert_eq!(body["data"]["sessions"][0]["user_id"], id.to_string());
        assert_eq!(body["data"]["sessions"][0]["revocable"], false);
    }
    let ambiguous = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/activity/sessions/shared-device/end",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(ambiguous.status(), StatusCode::CONFLICT);
    let ended = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/playback/activity/sessions/shared-device/end?user_id={alice_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(ended.status(), StatusCode::OK);
    let revoked = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/playback/devices/shared-device?user_id={alice_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(revoked).await["error"]["code"],
        "playback.unsupported"
    );
    let alice_report = send_playback(&app, &alice, media_id, "progress").await;
    assert_eq!(alice_report.status(), StatusCode::OK);
    assert_eq!(
        json_body(alice_report).await["data"]["ended_by_admin"],
        true
    );
    let bob_report = send_playback(&app, &bob, media_id, "progress").await;
    assert_eq!(bob_report.status(), StatusCode::OK);
    assert_eq!(json_body(bob_report).await["data"]["ended_by_admin"], false);
}

#[tokio::test]
async fn search_presets_history_and_ui_preferences_are_per_user() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (_alice_id, alice) = create_member(&app, "state-alice").await;
    let (_bob_id, bob) = create_member(&app, "state-bob").await;

    for (method, uri, body) in [
        (
            "GET",
            "/api/v1/search/torrents?keyword=alice-only",
            Value::Null,
        ),
        (
            "PUT",
            "/api/v1/search/presets",
            json!({ "presets": [{ "name": "Alice preset" }] }),
        ),
        (
            "PUT",
            "/api/v1/ui/preferences",
            json!({ "density": "compact" }),
        ),
    ] {
        let response = app
            .clone()
            .oneshot(request(method, uri, Some(&alice), body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    let bob_history = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/search/history",
                Some(&bob),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(bob_history["data"]["items"].as_array().unwrap().is_empty());
    let bob_presets = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/search/presets",
                Some(&bob),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        bob_presets["data"]["presets"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let bob_prefs = json_body(
        app.oneshot(request(
            "GET",
            "/api/v1/ui/preferences",
            Some(&bob),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(bob_prefs["data"], json!({}));
}
