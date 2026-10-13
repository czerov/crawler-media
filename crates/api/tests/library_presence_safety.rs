mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use domain::MediaKind;
use support::library_presence::PresenceFixture;
use tokio::sync::Notify;

#[tokio::test]
async fn prune_preserves_other_library_and_user_facts() {
    let fix = PresenceFixture::new();
    let (media_id, row) = fix.insert_media_and_ledger("target.mkv");

    // Set user playback progress and favorite
    let user_id = domain::UserId::new();
    {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard
            .set_playback_state(user_id, media_id, 3000, Some(true), Some(true), None, None)
            .unwrap();
    }

    // Verify to mark it missing
    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["data"]["missing"], 1);

    // Prune the missing row
    let (status, res) = fix
        .request(
            "DELETE",
            &format!("/api/v1/libraries/{}/missing-rows", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["data"]["deleted"], 1);

    // Verify ledger row is deleted
    let store_arc = fix.state.store();
    let guard = store_arc.lock();
    assert!(guard.get_ledger(&row.id.to_string()).unwrap().is_none());

    // User playback facts and Media record are PRESERVED (S1)
    assert!(guard.get_media(media_id).unwrap().is_some());
    assert_eq!(
        guard.playback_progress(user_id, media_id).unwrap(),
        Some(3000)
    );
}

#[tokio::test]
async fn frozen_or_deleted_library_rejects_presence_operations() {
    let fix = PresenceFixture::new();
    let (_media_id, _row) = fix.insert_media_and_ledger("target.mkv");

    // Freeze library
    {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.begin_library_removal(&fix.library_id).unwrap();
    }

    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(res["error"]["code"], "library.deleting");

    let (status, res) = fix
        .request(
            "DELETE",
            &format!("/api/v1/libraries/{}/missing-rows", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(res["error"]["code"], "library.deleting");
}
