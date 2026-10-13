#[path = "support/mod.rs"]
mod support;

use axum::http::StatusCode;
use domain::MediaKind;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn bounded_work_acceptance_full_batch_3_flow() {
    let fix = PresenceFixture::new();

    // 1. Scan 202 Operation
    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/scan", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let op_id = res["data"]["operation_id"].as_str().unwrap();

    // 2. Poll operation
    let (status, op_res) = fix
        .request(
            "GET",
            &format!("/api/v1/libraries/{}/scan-operations/{}", fix.library_id, op_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(op_res["data"]["operation_id"], op_id);

    // 3. Projection build
    let snap = fix
        .state
        .projection
        .ensure(fix.state.clone(), fix.library_id.clone())
        .await
        .unwrap();
    assert_eq!(snap.library_id, fix.library_id);

    // 4. Bounded budgets
    assert_eq!(fix.state.search_work.available_permits(), 4);
    assert_eq!(fix.state.fs_work.available_permits(), 2);
    assert_eq!(fix.state.http_work.available_permits(), 8);
}
