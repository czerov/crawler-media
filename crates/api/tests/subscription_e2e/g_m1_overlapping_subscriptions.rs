//! G-M1: two subscriptions of the same media in the same library must not
//! cover overlapping units (they would share ledger facts and wash each
//! other's files). A disjoint range is still allowed.

use serde_json::json;

use super::harness::*;

async fn create(h: &Harness, coverage: serde_json::Value) -> axum::http::StatusCode {
    let payload = json!({
        "media": { "kind": "tv", "title": "The Long Watch", "tmdb_id": "100" },
        "coverage": coverage,
        "fetch_mode": "search",
        "wash_cut": false,
        "full_season_pack": false
    });
    tower::ServiceExt::oneshot(
        h.app.clone(),
        super::common::request("POST", "/api/v1/subscriptions", Some(TOKEN), payload),
    )
    .await
    .unwrap()
    .status()
}

#[tokio::test]
async fn overlapping_subscription_is_rejected_disjoint_is_allowed() {
    let h = Harness::new();
    let first = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 5 });
    assert_eq!(create(&h, first).await, axum::http::StatusCode::CREATED);
    let overlap = json!({ "kind": "tv", "season": 1, "episode_from": 5, "episode_to": 8 });
    assert_eq!(create(&h, overlap).await, axum::http::StatusCode::CONFLICT);
    let disjoint = json!({ "kind": "tv", "season": 1, "episode_from": 6, "episode_to": 8 });
    assert_eq!(create(&h, disjoint).await, axum::http::StatusCode::CREATED);
    let other_season = json!({ "kind": "tv", "season": 2, "episode_from": 1 });
    assert_eq!(
        create(&h, other_season).await,
        axum::http::StatusCode::CREATED
    );
}
