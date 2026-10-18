//! L4: the "订阅已完成" marker resets when the subscription is no longer
//! complete, and is removed with the subscription.

use serde_json::json;

use super::fake_catalog::tv_subscribe;
use super::harness::*;

const E01: &str = "The.Long.Watch.S01E01.1080p.WEB-DL.x264-GRP";

#[tokio::test]
async fn completion_marker_resets_and_is_deleted_with_subscription() {
    let h = Harness::new();
    h.create_site().await;
    let window = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 1 });
    let id = tv_subscribe(&h, window, json!({})).await;
    let key = format!("subscribe.completed:{id}");
    let marker = |h: &Harness| h.state.store().lock().get_setting(&key).unwrap();
    h.set_search_rows(&[(E01, 1)]);
    h.scheduled_search(id).await.unwrap();
    let video = probed_video(1080, "h264", false);
    h.qb.complete(0, E01, &[(&format!("{E01}.mkv"), &video)]);
    h.transfer(id).await.unwrap();
    assert!(marker(&h).is_some(), "notified once complete");

    for file in h.library_files() {
        if file.extension().is_some_and(|e| e == "mkv") {
            std::fs::remove_file(file).unwrap();
        }
    }
    h.set_search_rows(&[]);
    h.scheduled_search(id).await.unwrap();
    assert!(marker(&h).is_none(), "marker reset when an episode is gone");

    h.state.store().lock().put_setting(&key, "1").unwrap();
    assert!(h.state.store().lock().delete_subscribe(id).unwrap());
    assert!(marker(&h).is_none(), "no marker left after delete");
}
