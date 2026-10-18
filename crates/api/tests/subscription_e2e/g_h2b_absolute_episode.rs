//! G-H2b: an absolute-numbered title (no season marker) is mapped through the
//! catalog's per-season episode counts onto the right season and episode.

use serde_json::json;

use super::fake_catalog::{FakeCatalog, date_in, tv_subscribe};
use super::harness::*;

const ABSOLUTE: &str = "The Long Watch Episode 13 1080p WEB-DL x264-GRP";

fn two_seasons() -> std::sync::Arc<FakeCatalog> {
    let mut rows: Vec<_> = (1..=12).map(|ep| (1, ep, Some(date_in(-60)))).collect();
    rows.extend((1..=10).map(|ep| (2, ep, Some(date_in(-5)))));
    FakeCatalog::with_episodes(&rows)
}

#[tokio::test]
async fn absolute_episode_lands_in_later_season() {
    let h = Harness::with_catalog(two_seasons());
    h.create_site().await;
    let window = json!({ "kind": "tv", "season": 2, "episode_from": 1, "episode_to": 2 });
    let id = tv_subscribe(&h, window, json!({})).await;
    h.set_search_rows(&[(ABSOLUTE, 1)]);
    h.scheduled_search(id).await.unwrap();
    assert_eq!(h.pending(id).len(), 1, "Episode 13 is S02E01");
    let video = probed_video(1080, "h264", false);
    h.qb.complete(0, ABSOLUTE, &[("The Long Watch - 13 1080p.mkv", &video)]);
    h.transfer(id).await.unwrap();
    let files: Vec<_> = h
        .library_files()
        .into_iter()
        .filter(|p| super::is_video(p))
        .map(|p| p.display().to_string())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].contains("S02E01"), "{files:?}");
    let facts = h.state.store().lock().load_subscribe_facts(id).unwrap();
    assert!(facts.get(Some(2), Some(1)).is_some());
}
