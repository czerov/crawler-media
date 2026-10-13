#[path = "support/mod.rs"]
mod support;

use axum::http::StatusCode;
use domain::MediaKind;
use serde_json::Value;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn browse_semantic_parity_smoke() {
    let fix = PresenceFixture::new();

    // Insert deterministic test items
    let (_m1, r1) = fix.insert_media_and_ledger("Alpha (2020).mkv");
    let (_m2, r2) = fix.insert_media_and_ledger("Beta (2024).mkv");

    // Fetch items listing
    let (status, res) = fix
        .request(
            "GET",
            &format!("/api/v1/libraries/{}/items", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["ok"], true);

    let items = res["data"].as_array().expect("items must be array");
    assert_eq!(items.len(), 2);
}
