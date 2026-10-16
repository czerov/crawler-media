//! D-M5: manual submission into a subscription respects owned slots (409
//! unless force), scores the torrent with the subscription filter, and
//! rejects an invalid site id.

use serde_json::{Value, json};

use super::fake_catalog::tv_subscribe;
use super::harness::*;

const E01: &str = "The.Long.Watch.S01E01.1080p.WEB-DL.x264-GRP";
const E01_ALT: &str = "The.Long.Watch.S01E01.1080p.WEB-DL.x264-ALT";

async fn submit(h: &Harness, body: Value) -> (axum::http::StatusCode, ()) {
    let response = tower::ServiceExt::oneshot(
        h.app.clone(),
        super::common::request("POST", "/api/v1/downloaders/submit", Some(TOKEN), body),
    )
    .await
    .unwrap();
    (response.status(), ())
}

#[tokio::test]
async fn manual_submit_conflicts_on_owned_slot_and_scores_with_filter() {
    let h = Harness::new();
    h.create_site().await;
    let window = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 });
    let id = tv_subscribe(&h, window, json!({})).await;
    let video = probed_video(1080, "h264", false);
    h.set_search_rows(&[(E01, 1)]);
    h.scheduled_search(id).await.unwrap();
    h.qb.complete(0, E01, &[(&format!("{E01}.mkv"), &video)]);
    h.transfer(id).await.unwrap();

    let body = json!({ "title": E01_ALT, "download_url": "https://pt.example/alt.torrent",
        "subscribe_id": id.to_string() });
    let (status, _) = submit(&h, body.clone()).await;
    assert_eq!(
        status,
        axum::http::StatusCode::CONFLICT,
        "E01 already owned"
    );
    assert_eq!(h.qb.add_count(), 1, "nothing submitted");

    let mut forced = body;
    forced["force"] = json!(true);
    let (status, _) = submit(&h, forced).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let pending = h.pending(id);
    assert_eq!(pending.len(), 1);
    assert!(
        pending[0].0 > 0,
        "pending scored by the filter, got {}",
        pending[0].0
    );

    let (status, _) = submit(
        &h,
        json!({ "title": E01_ALT, "download_url": "https://pt.example/x.torrent", "site_id": "not-a-site" }),
    )
    .await;
    assert_eq!(
        status,
        axum::http::StatusCode::BAD_REQUEST,
        "invalid site id"
    );
}
