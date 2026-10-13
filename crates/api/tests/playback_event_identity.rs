#[path = "support/mod.rs"]
mod support;

use axum::http::StatusCode;
use domain::LedgerId;
use serde_json::json;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn late_old_stop_does_not_close_new_session() {
    let fix = PresenceFixture::new();
    let (_, row_a) = fix.insert_media_and_ledger("movie_a.mkv");
    let (_, row_b) = fix.insert_media_and_ledger("movie_b.mkv");

    let user_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };

    // 1. Start session A
    let session_id_a = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard
            .issue_play_session(user_id, "dev1", row_a.id, 1000)
            .unwrap()
    };

    // Start A with play_session_id_a
    let (status, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev1",
                "media_item_id": row_a.media_id.to_string(),
                "file_id": row_a.id.to_string(),
                "event": "start",
                "position_ms": 100,
                "play_session_id": session_id_a,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // 2. Start session B on same device
    let session_id_b = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard
            .issue_play_session(user_id, "dev1", row_b.id, 2000)
            .unwrap()
    };
    let (status, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev1",
                "media_item_id": row_b.media_id.to_string(),
                "file_id": row_b.id.to_string(),
                "event": "start",
                "position_ms": 200,
                "play_session_id": session_id_b,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // Verify active session is B
    let store_arc = fix.state.store();
    let current = store_arc.lock().get_session(user_id, "dev1").unwrap().unwrap();
    assert_eq!(current.ledger_id.as_deref(), Some(&row_b.id.to_string()[..]));
    assert_eq!(current.play_session_id.as_deref(), Some(session_id_b.as_str()));

    // 3. Late STOP from session A arrives!
    let (status, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev1",
                "media_item_id": row_a.media_id.to_string(),
                "file_id": row_a.id.to_string(),
                "event": "stop",
                "position_ms": 150,
                "play_session_id": session_id_a,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // Session B must NOT be closed!
    let current_after = store_arc.lock().get_session(user_id, "dev1").unwrap();
    assert!(current_after.is_some(), "Session B must still be active");
    assert_eq!(
        current_after.unwrap().play_session_id.as_deref(),
        Some(session_id_b.as_str())
    );
}

#[tokio::test]
async fn late_old_progress_does_not_rewind_position() {
    let fix = PresenceFixture::new();
    let (_, row) = fix.insert_media_and_ledger("same_movie.mkv");
    let user_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };

    // Session 1 on same file
    let id_1 = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.issue_play_session(user_id, "dev1", row.id, 1000).unwrap()
    };
    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "dev1",
            "media_item_id": row.media_id.to_string(),
            "file_id": row.id.to_string(),
            "event": "start",
            "position_ms": 5000,
            "play_session_id": id_1,
        }),
    ).await;

    // Session 2 on same file (newer generation)
    let id_2 = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.issue_play_session(user_id, "dev1", row.id, 2000).unwrap()
    };
    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "dev1",
            "media_item_id": row.media_id.to_string(),
            "file_id": row.id.to_string(),
            "event": "start",
            "position_ms": 8000,
            "play_session_id": id_2,
        }),
    ).await;

    // Late progress from session 1 with position 2000 arrives
    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "dev1",
            "media_item_id": row.media_id.to_string(),
            "file_id": row.id.to_string(),
            "event": "progress",
            "position_ms": 2000,
            "play_session_id": id_1,
        }),
    ).await;

    // Active session position must remain 8000, not rewound to 2000
    let store_arc = fix.state.store();
    let cur = store_arc.lock().get_session(user_id, "dev1").unwrap().unwrap();
    assert_eq!(cur.position_ms, 8000);
}
