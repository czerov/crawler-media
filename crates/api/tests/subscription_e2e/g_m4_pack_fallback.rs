//! G-M4: a full-season-pack subscription with `pack_fallback_after_days`
//! falls back to single episodes once it has waited long enough without a pack.

use serde_json::json;

use super::fake_catalog::tv_subscribe;
use super::harness::*;

const E01: &str = "The.Long.Watch.S01E01.1080p.WEB-DL.x264-GRP";
const E02: &str = "The.Long.Watch.S01E02.1080p.WEB-DL.x264-GRP";

fn waited(h: &Harness, id: domain::SubscribeId, days: i64) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    h.state
        .store()
        .lock()
        .put_setting(
            &format!("subscribe.pack_wait_since:{id}"),
            &(now - days * 86_400).to_string(),
        )
        .unwrap();
}

async fn setup(extra: serde_json::Value) -> (Harness, domain::SubscribeId) {
    let h = Harness::new();
    h.create_site().await;
    let window = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 });
    let mut extra = extra;
    extra["full_season_pack"] = json!(true);
    let id = tv_subscribe(&h, window, extra).await;
    h.set_search_rows(&[(E01, 1), (E02, 2)]);
    (h, id)
}

#[tokio::test]
async fn falls_back_to_singles_after_the_wait() {
    let (h, id) = setup(json!({ "pack_fallback_after_days": 3 })).await;
    waited(&h, id, 1);
    h.scheduled_search(id).await.unwrap();
    assert!(h.pending(id).is_empty(), "day 1: still waiting for a pack");
    waited(&h, id, 4);
    h.scheduled_search(id).await.unwrap();
    let mut titles: Vec<_> = h
        .pending(id)
        .into_iter()
        .map(|(_, p)| p.torrent.title)
        .collect();
    titles.sort();
    assert_eq!(titles, vec![E01.to_string(), E02.to_string()]);
    let video = probed_video(1080, "h264", false);
    h.qb.complete(0, E01, &[(&format!("{E01}.mkv"), &video)]);
    h.transfer(id).await.unwrap();
    assert!(
        h.library_files()
            .iter()
            .any(|p| p.to_string_lossy().contains("E01")),
        "single episode imported: {:?}",
        h.library_files()
    );
}

#[tokio::test]
async fn without_fallback_only_a_pack_is_taken() {
    let (h, id) = setup(json!({})).await;
    waited(&h, id, 30);
    h.scheduled_search(id).await.unwrap();
    assert!(h.pending(id).is_empty());
}
