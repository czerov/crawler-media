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
