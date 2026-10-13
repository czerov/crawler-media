#[path = "support/mod.rs"]
mod support;

use axum::http::StatusCode;
use domain::MediaKind;
use serde_json::json;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn full_lifecycle_playback_and_scan_correctness_acceptance() {
    let fix = PresenceFixture::new();

    // 1. Scan file into library
    let movie_file = fix.root.join("correctness_movie (2024).mkv");
    std::fs::write(&movie_file, b"content").unwrap();

    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/scan", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["ok"], true);
    assert_eq!(res["data"]["inserted"], 1);

    let store_arc = fix.state.store();
    let row = store_arc
        .lock()
        .ledger_by_path(&movie_file.display().to_string())
        .unwrap()
        .unwrap();

    // 2. Playback session generation isolation and watch time accumulation
    let user_id = store_arc.lock().user_by_token(&fix.admin_token).unwrap().unwrap().id;
    let session_id_1 = store_arc
        .lock()
        .issue_play_session(user_id, "device_acc", row.id, 1000)
        .unwrap();

    // Start session 1 at t=1000s, pos=0
    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "device_acc",
            "media_item_id": row.media_id.to_string(),
            "file_id": row.id.to_string(),
            "event": "start",
            "position_ms": 0,
            "play_session_id": session_id_1,
        }),
    ).await;

    // Normal progress at pos=5000ms
    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "device_acc",
            "media_item_id": row.media_id.to_string(),
            "file_id": row.id.to_string(),
            "event": "progress",
            "position_ms": 5000,
            "play_session_id": session_id_1,
        }),
    ).await;

    // Start session 2 (new generation) at t=2000s, pos=0
    let session_id_2 = store_arc
        .lock()
        .issue_play_session(user_id, "device_acc", row.id, 2000)
        .unwrap();

    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "device_acc",
            "media_item_id": row.media_id.to_string(),
            "file_id": row.id.to_string(),
            "event": "start",
            "position_ms": 0,
            "play_session_id": session_id_2,
        }),
    ).await;

    // Late stop from session 1 arrives
    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "device_acc",
            "media_item_id": row.media_id.to_string(),
            "file_id": row.id.to_string(),
            "event": "stop",
            "position_ms": 5000,
            "play_session_id": session_id_1,
        }),
    ).await;

    // Active session is still session 2!
    let cur = store_arc.lock().get_session(user_id, "device_acc").unwrap().unwrap();
    assert_eq!(cur.play_session_id.as_deref(), Some(session_id_2.as_str()));

    // 3. Obligations dispatched
    let _ = api::scan_probe_dispatch::dispatch_once(fix.state.clone()).await;
    let obs = store_arc.lock().scan_probe_obligations(10).unwrap();
    assert_eq!(obs.len(), 0);
}
