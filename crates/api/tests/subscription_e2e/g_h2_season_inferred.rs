//! G-H2: a title without a season marker parses as S01 by default. For a
//! multi-season show whose catalog S01 has fewer episodes, such an episode
//! number must not be placed into an S01 slot.

use serde_json::json;

use super::fake_catalog::{FakeCatalog, date_in, tv_subscribe};
use super::harness::*;

const ABSOLUTE: &str = "The Long Watch Episode 13 1080p WEB-DL x264-GRP";
const EXPLICIT: &str = "The.Long.Watch.S01E12.1080p.WEB-DL.x264-GRP";

fn twelve_episode_s01() -> std::sync::Arc<FakeCatalog> {
    let mut rows: Vec<_> = (1..=12).map(|ep| (1, ep, Some(date_in(-30)))).collect();
    rows.push((2, 1, Some(date_in(-1))));
    FakeCatalog::with_episodes(&rows)
}

#[tokio::test]
async fn inferred_season_episode_beyond_catalog_s01_is_not_submitted() {
    let h = Harness::with_catalog(twelve_episode_s01());
    h.create_site().await;
    let window = json!({ "kind": "tv", "season": 1, "episode_from": 12, "episode_to": 13 });
    let id = tv_subscribe(&h, window, json!({})).await;
    h.set_search_rows(&[(ABSOLUTE, 1), (EXPLICIT, 2)]);
    h.scheduled_search(id).await.unwrap();
    let pending = h.pending(id);
    let titles: Vec<_> = pending.iter().map(|(_, p)| p.torrent.title.clone()).collect();
    assert_eq!(titles, vec![EXPLICIT.to_string()], "absolute-numbered E13 is not S01E13");
}

#[tokio::test]
async fn inferred_season_still_matches_single_season_show() {
    let rows: Vec<_> = (1..=13).map(|ep| (1, ep, Some(date_in(-30)))).collect();
    let h = Harness::with_catalog(FakeCatalog::with_episodes(&rows));
    h.create_site().await;
    let window = json!({ "kind": "tv", "season": 1, "episode_from": 13, "episode_to": 13 });
    let id = tv_subscribe(&h, window, json!({})).await;
    h.set_search_rows(&[(ABSOLUTE, 1)]);
    h.scheduled_search(id).await.unwrap();
    assert_eq!(h.pending(id).len(), 1);
}
