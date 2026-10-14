#[path = "support/mod.rs"]
mod support;

use axum::http::StatusCode;
use domain::MediaKind;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn scan_returns_202_with_operation_id_and_polls() {
    let fix = PresenceFixture::new();

    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/scan", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(res["ok"], true);
    assert_eq!(res["data"]["queued"], true);

    let op_id = res["data"]["operation_id"].as_str().expect("must return operation_id");

    // Poll operation status
    let (poll_status, poll_res) = fix
        .request(
            "GET",
            &format!("/api/v1/libraries/{}/scan-operations/{}", fix.library_id, op_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(poll_status, StatusCode::OK);
    assert_eq!(poll_res["ok"], true);
    assert_eq!(poll_res["data"]["operation_id"], op_id);
}

#[tokio::test]
async fn cancel_scan_does_not_break_subsequent_library_work_leases() {
    let fix = PresenceFixture::new();

    // 1. Enqueue scan
    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/scan", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let op_id = res["data"]["operation_id"].as_str().unwrap();

    // 2. Cancel scan with wrong library id -> 404
    let (wrong_status, _) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/wrong-lib-id/scan-operations/{}/cancel", op_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(wrong_status, StatusCode::NOT_FOUND);

    // 3. Cancel scan with correct library id -> 200
    let (cancel_status, cancel_res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/scan-operations/{}/cancel", fix.library_id, op_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(cancel_status, StatusCode::OK);
    assert_eq!(cancel_res["ok"], true);

    // 4. Critical: library work lease must NOT be permanently broken!
    let lease_res = fix.state.acquire_library_work(&fix.library_id);
    assert!(lease_res.is_ok(), "Canceling a scan must not freeze the library permanently");

    // 5. Subsequent scan must be accepted and **actually complete** — a
    //    cancelled shared control would leave it failed/partial.
    let next_res = fix.scan_and_wait_completed(&fix.library_id).await;
    assert_eq!(next_res["status"], "completed");
}
