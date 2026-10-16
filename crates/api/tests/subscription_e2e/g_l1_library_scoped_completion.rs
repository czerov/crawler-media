//! G-L1: the completion notice uses library-scoped facts: files outside the
//! subscription's target library do not count.

use serde_json::json;

use super::fake_catalog::tv_subscribe;
use super::harness::*;

const E01: &str = "The.Long.Watch.S01E01.1080p.WEB-DL.x264-GRP";

#[tokio::test]
async fn facts_outside_the_target_library_do_not_complete() {
    let h = Harness::new();
    h.create_site().await;
    let window = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 1 });
    let id = tv_subscribe(&h, window, json!({})).await;
    let key = format!("subscribe.completed:{id}");
    h.set_search_rows(&[(E01, 1)]);
    h.scheduled_search(id).await.unwrap();
    let video = probed_video(1080, "h264", false);
    h.qb.complete(0, E01, &[(&format!("{E01}.mkv"), &video)]);
    h.transfer(id).await.unwrap();
    assert!(h.state.store().lock().get_setting(&key).unwrap().is_some());

    // Switch the subscription to another (empty) TV library.
    let other_root = tempfile::tempdir().unwrap();
    {
        let store = h.state.store();
        let store = store.lock();
        let library = store
            .create_library(
                domain::MediaKind::Tv,
                "其它剧集",
                &[other_root.path().to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let mut subscribe = store.get_subscribe(id).unwrap().unwrap();
        subscribe.library_id = Some(library.id.to_string().parse().unwrap());
        subscribe.tracking_state = "active".into();
        store.update_subscribe(&subscribe).unwrap();
        store.delete_setting(&key).unwrap();
    }
    h.set_search_rows(&[]);
    h.scheduled_search(id).await.unwrap();
    assert!(
        h.state.store().lock().get_setting(&key).unwrap().is_none(),
        "no completion notice for files in another library"
    );
}
