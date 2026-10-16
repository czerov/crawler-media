//! G-H1: scheduled searches for episodes that have not aired yet must not hit
//! the indexer; once the air date has passed the episode is searched normally.

use serde_json::json;

use super::fake_catalog::{FakeCatalog, date_in, tv_subscribe};
use super::harness::*;

const E01: &str = "The.Long.Watch.S01E01.1080p.WEB-DL.x264-GRP";

#[tokio::test]
async fn unaired_open_window_skips_indexer_until_air_date() {
    let catalog = FakeCatalog::with_episodes(&[(1, 1, Some(date_in(3)))]);
    let h = Harness::with_catalog(catalog.clone());
    h.create_site().await;
    let open = json!({ "kind": "tv", "season": 1, "episode_from": 1 });
    let id = tv_subscribe(&h, open, json!({})).await;
    h.set_search_rows(&[(E01, 1)]);

    let before = h.search_requests();
    h.scheduled_search(id).await.unwrap();
    assert_eq!(
        h.search_requests(),
        before,
        "E01 airs in 3 days: no indexer request"
    );
    assert_eq!(h.qb.add_count(), 0);

    // Air date reached (catalog now reports yesterday).
    *catalog.episodes.lock() = vec![(1, 1, Some(date_in(-1)))];
    h.scheduled_search(id).await.unwrap();
    assert!(h.search_requests() > before, "aired: searched");
    assert_eq!(h.qb.add_count(), 1, "E01 submitted after airing");
}

#[tokio::test]
async fn missing_catalog_data_keeps_searching() {
    let h = Harness::with_catalog(FakeCatalog::with_episodes(&[]));
    h.create_site().await;
    let window = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 });
    let id = tv_subscribe(&h, window, json!({})).await;
    h.set_search_rows(&[(E01, 1)]);
    let before = h.search_requests();
    h.scheduled_search(id).await.unwrap();
    assert!(h.search_requests() > before);
    assert_eq!(h.qb.add_count(), 1);
}
