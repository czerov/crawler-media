#[path = "support/mod.rs"]
mod support;

use axum::http::StatusCode;
use domain::MediaKind;
use serde_json::Value;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn browse_items_pages_cleanly_with_projection() {
    let fix = PresenceFixture::new();

    // Insert 5 items
    for i in 1..=5 {
        fix.insert_media_and_ledger(&format!("Item_{i} (2024).mkv"));
    }

    // Ensure projection
    let snap = fix
        .state
        .projection
        .ensure(fix.state.clone(), fix.library_id.clone())
        .await
        .unwrap();
    assert_eq!(snap.media.len(), 5);

    // Fetch items with limit 2
    let (status, res) = fix
        .request(
            "GET",
            &format!("/api/v1/libraries/{}/items?limit=2&offset=0", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["ok"], true);

    let items = res["data"].as_array().expect("items must be array");
    assert_eq!(items.len(), 2);
}
