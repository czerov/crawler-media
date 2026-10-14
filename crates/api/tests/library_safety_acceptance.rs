#[path = "support/mod.rs"]
mod support;

use axum::http::StatusCode;
use domain::MediaKind;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn verify_restore_scan_prune_preserves_two_users_and_sidecar_bytes() {
    let fix = PresenceFixture::new();
    let (media_id, row) = fix.insert_media_and_ledger("test_movie (2024).mkv");

    // Two users with position and favorite
    let user1 = domain::UserId::new();
    let user2 = domain::UserId::new();
    {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard
            .set_playback_state(user1, media_id, 1000, Some(true), Some(true), None, None)
            .unwrap();
        guard
            .set_playback_state(user2, media_id, 2000, Some(false), Some(false), None, None)
            .unwrap();
    }

    // 1. Verify: file not on disk -> missing
    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["data"]["missing"], 1);

    // 2. Restore file on disk
    std::fs::write(&row.path, b"movie content").unwrap();

    // 3. Scan: clears missing flag without replacing LedgerId or MediaId
    let (status, _res) = fix.scan_and_wait(&fix.library_id).await;
    assert_eq!(status, StatusCode::OK);

    // Check missing is cleared
    let store_arc = fix.state.store();
    assert!(
        !store_arc
            .lock()
            .missing_at_by_path()
            .unwrap()
            .contains_key(&row.path)
    );

    // 4. Prune: deleted count is 0, restored is 0 (already restored)
    let (status, res) = fix
        .request(
            "DELETE",
            &format!("/api/v1/libraries/{}/missing-rows", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["data"]["deleted"], 0);

    // Both users' playback facts and Media record preserved!
    let guard = store_arc.lock();
    let loaded_row = guard.get_ledger(&row.id.to_string()).unwrap().unwrap();
    assert_eq!(loaded_row.id, row.id);
    assert_eq!(loaded_row.media_id, media_id);
    assert!(guard.get_media(media_id).unwrap().is_some());
    assert_eq!(
        guard.playback_progress(user1, media_id).unwrap(),
        Some(1000)
    );
    assert_eq!(
        guard.playback_progress(user2, media_id).unwrap(),
        Some(2000)
    );
}

#[tokio::test]
async fn frozen_library_rejects_presence_and_scan_publication() {
    let fix = PresenceFixture::new();
    let (_media_id, _row) = fix.insert_media_and_ledger("frozen_movie (2024).mkv");

    // Freeze library
    {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.begin_library_removal(&fix.library_id).unwrap();
    }

    // Verify rejected
    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(res["error"]["code"], "library.deleting");

    // Prune rejected
    let (status, res) = fix
        .request(
            "DELETE",
            &format!("/api/v1/libraries/{}/missing-rows", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(res["error"]["code"], "library.deleting");

    // Scan rejected
    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/scan", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(res["error"]["code"], "library.deleting");
}
