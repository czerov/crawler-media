//! D-L2: per-pending qBittorrent lookups filter `torrents/info` by tag/hash
//! instead of listing every task in the client.

use std::sync::atomic::Ordering;

use serde_json::json;

use super::fake_catalog::tv_subscribe;
use super::harness::*;
use super::mock_qbit::Task;

const E01: &str = "The.Long.Watch.S01E01.1080p.WEB-DL.x264-GRP";
const E02: &str = "The.Long.Watch.S01E02.1080p.WEB-DL.x264-GRP";

#[tokio::test]
async fn transfer_round_does_not_list_all_tasks_per_pending() {
    let h = Harness::new();
    h.create_site().await;
    let window = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 });
    let id = tv_subscribe(&h, window, json!({})).await;
    h.set_search_rows(&[(E01, 1), (E02, 2)]);
    h.scheduled_search(id).await.unwrap();
    assert_eq!(h.pending(id).len(), 2);
    for i in 0..200 {
        h.qb.inject(Task {
            hash: format!("{i:040x}"),
            name: format!("someone-else-{i}"),
            tags: vec![],
            progress: 1.0,
            save_path: h.qb.save_root.display().to_string(),
            files: vec![],
            size: 1,
            state: "uploading".into(),
            source: "manual".into(),
        });
    }
    h.qb.unfiltered_info_calls.store(0, Ordering::SeqCst);
    h.transfer(id).await.unwrap();
    let unfiltered = h.qb.unfiltered_info_calls.load(Ordering::SeqCst);
    // One full snapshot per downloader (reconcile) is allowed; per-pending
    // lookups must be filtered.
    assert!(
        unfiltered <= 1,
        "unfiltered torrents/info calls: {unfiltered}"
    );
}
