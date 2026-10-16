//! M5: `follow_future=false` keeps an open window to aired episodes only;
//! `true` keeps tracking the next (unaired) episode as before.

use serde_json::json;

use super::fake_catalog::{FakeCatalog, date_in, tv_subscribe};
use super::harness::*;

async fn tracks_e04(follow: bool) -> bool {
    let mut rows: Vec<_> = (1..=3).map(|ep| (1, ep, Some(date_in(-10)))).collect();
    rows.push((1, 4, Some(date_in(5))));
    let h = Harness::with_catalog(FakeCatalog::with_episodes(&rows));
    h.create_site().await;
    let window = json!({ "kind": "tv", "season": 1, "episode_from": 1 });
    let id = tv_subscribe(&h, window, json!({ "follow_future": follow })).await;
    h.set_search_rows(&[
        ("The.Long.Watch.S01E01.1080p.WEB-DL.x264-GRP", 1),
        ("The.Long.Watch.S01E02.1080p.WEB-DL.x264-GRP", 2),
        ("The.Long.Watch.S01E03.1080p.WEB-DL.x264-GRP", 3),
    ]);
    h.scheduled_search(id).await.unwrap();
    let history = h.state.store().lock().load_wanted_history(id).unwrap();
    history
        .get(&(Some(1), Some(4)))
        .is_some_and(|row| row.search_attempts > 0)
}

#[tokio::test]
async fn follow_future_false_stops_at_aired_episodes() {
    assert!(!tracks_e04(false).await, "E04 (unaired) not tracked");
}

#[tokio::test]
async fn follow_future_true_tracks_next_episode() {
    assert!(tracks_e04(true).await, "E04 tracked as the next episode");
}
