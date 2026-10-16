//! M4a: a bounded, non-wash subscription whose every unit is in the library
//! becomes `completed` and stops hitting indexers; losing a file revives it.

use serde_json::json;

use super::fake_catalog::tv_subscribe;
use super::harness::*;

const E01: &str = "The.Long.Watch.S01E01.1080p.WEB-DL.x264-GRP";
const E02: &str = "The.Long.Watch.S01E02.1080p.WEB-DL.x264-GRP";

fn state_of(h: &Harness, id: domain::SubscribeId) -> String {
    h.state
        .store()
        .lock()
        .get_subscribe(id)
        .unwrap()
        .unwrap()
        .tracking_state
}

#[tokio::test]
async fn completed_subscription_stops_searching_and_revives_on_loss() {
    let h = Harness::new();
    h.create_site().await;
    let window = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 });
    let id = tv_subscribe(&h, window, json!({})).await;
    let video = probed_video(1080, "h264", false);
    h.set_search_rows(&[(E01, 1), (E02, 2)]);
    h.scheduled_search(id).await.unwrap();
    h.qb.complete(0, E01, &[(&format!("{E01}.mkv"), &video)]);
    h.qb.complete(1, E02, &[(&format!("{E02}.mkv"), &video)]);
    h.transfer(id).await.unwrap();
    assert_eq!(state_of(&h, id), "completed");

    let before = h.search_requests();
    for _ in 0..3 {
        h.scheduled_search(id).await.unwrap();
    }
    assert_eq!(
        h.search_requests(),
        before,
        "no indexer requests once completed"
    );

    let e02 = h
        .library_files()
        .into_iter()
        .find(|p| super::is_video(p) && p.to_string_lossy().contains("E02"))
        .unwrap();
    std::fs::remove_file(&e02).unwrap();
    h.qb.remove_all();
    h.scheduled_search(id).await.unwrap();
    assert_eq!(state_of(&h, id), "active", "revived after losing E02");
    assert!(h.search_requests() > before, "searches again");
}
