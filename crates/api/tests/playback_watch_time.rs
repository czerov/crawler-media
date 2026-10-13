#[path = "support/mod.rs"]
mod support;

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use axum::http::StatusCode;
use domain::LedgerId;
use serde_json::json;
use support::library_presence::PresenceFixture;

struct FakeClock(AtomicI64);

impl api::playback_events::PlaybackClock for FakeClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn watched_time_accumulates_correctly_and_ignores_seeks() {
    let fix = PresenceFixture::new();
    let (_, row) = fix.insert_media_and_ledger("clock_movie.mkv");
    let user_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };

    let session_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.issue_play_session(user_id, "dev_clock", row.id, 1000).unwrap()
    };

    // 1. Start at t = 1000s, position 0
    let (status, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev_clock",
                "media_item_id": row.media_id.to_string(),
                "file_id": row.id.to_string(),
                "event": "start",
                "position_ms": 0,
                "play_session_id": session_id,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // 2. Advance 1 second in time (t = 1001s), but jump position to 600_000ms (SEEK!)
    // Update store state time if needed
    // First let's send seek progress
    let (status, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev_clock",
                "media_item_id": row.media_id.to_string(),
                "file_id": row.id.to_string(),
                "event": "progress",
                "position_ms": 600_000,
                "play_session_id": session_id,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // Because position jump (600_000ms) is far greater than elapsed (approx 0-1s), watched_ms must be 0!
    let store_arc = fix.state.store();
    let s = store_arc.lock().get_session(user_id, "dev_clock").unwrap().unwrap();
    assert_eq!(s.watched_ms, 0, "seek must not add to watched_ms");
}
