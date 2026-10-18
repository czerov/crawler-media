//! L1: a tiny bare `S01` release is not taken as a full season pack.

use serde_json::json;

use super::fake_catalog::tv_subscribe;
use super::harness::*;

const SP: &str = "The.Long.Watch.S01.SP.1080p.WEB-DL.x264-GRP";
const E01: &str = "The.Long.Watch.S01E01.1080p.WEB-DL.x264-GRP";
const PACK: &str = "The.Long.Watch.S01.1080p.WEB-DL.x264-GRP";

async fn submitted(rows: &[(&str, u32, &str)]) -> Vec<String> {
    let h = Harness::new();
    h.create_site().await;
    let window = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 10 });
    let id = tv_subscribe(&h, window, json!({ "full_season_pack": true })).await;
    h.set_search_rows_sized(rows);
    h.scheduled_search(id).await.unwrap();
    h.pending(id)
        .into_iter()
        .map(|(_, p)| p.torrent.title)
        .collect()
}

#[tokio::test]
async fn small_bare_season_is_not_submitted() {
    let titles = submitted(&[(SP, 1, "300.00 MB"), (E01, 2, "2.00 GB")]).await;
    assert!(titles.is_empty(), "{titles:?}");
}

#[tokio::test]
async fn full_size_bare_season_is_still_a_pack() {
    let titles = submitted(&[(PACK, 1, "18.00 GB"), (E01, 2, "2.00 GB")]).await;
    assert_eq!(titles, vec![PACK.to_string()]);
}
