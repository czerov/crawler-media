//! G-M3: with `auto_next_season`, a completed single-season subscription
//! creates exactly one follow-up subscription for the next catalog season.

use serde_json::json;

use super::fake_catalog::{FakeCatalog, date_in, tv_subscribe};
use super::harness::*;

const E01: &str = "The.Long.Watch.S01E01.1080p.WEB-DL.x264-GRP";

async fn run(auto: bool) -> Vec<domain::Subscribe> {
    let rows = vec![(1, 1, Some(date_in(-30))), (2, 1, Some(date_in(-1)))];
    let h = Harness::with_catalog(FakeCatalog::with_episodes(&rows));
    h.create_site().await;
    let window = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 1 });
    let id = tv_subscribe(&h, window, json!({ "auto_next_season": auto })).await;
    h.set_search_rows(&[(E01, 1)]);
    h.scheduled_search(id).await.unwrap();
    let video = probed_video(1080, "h264", false);
    h.qb.complete(0, E01, &[(&format!("{E01}.mkv"), &video)]);
    h.transfer(id).await.unwrap();
    h.transfer(id).await.unwrap(); // idempotent
    h.state
        .store()
        .lock()
        .list_all_subscribes()
        .unwrap()
        .into_iter()
        .filter(|s| s.id != id)
        .collect()
}

#[tokio::test]
async fn completed_season_creates_next_season_once() {
    let others = run(true).await;
    assert_eq!(others.len(), 1, "exactly one follow-up");
    assert!(matches!(
        others[0].coverage,
        domain::Coverage::Tv {
            season: 2,
            episode_from: 1,
            episode_to: None
        }
    ));
}

#[tokio::test]
async fn auto_next_season_off_by_default() {
    assert!(run(false).await.is_empty());
}
