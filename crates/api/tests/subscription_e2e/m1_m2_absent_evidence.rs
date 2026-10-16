//! Review M-1 / M-2: task-center cleanup uses the persisted info hash, and an
//! empty client list is no evidence that HTTP torrents are gone.

use super::harness::*;
use super::mock_qbit::Task;

fn other_owned_task() -> Task {
    Task {
        hash: "f".repeat(40),
        name: "Other.Show.S01E01".into(),
        tags: vec![
            "crawler-media".into(),
            downloader::ownership_tag("https://pt.example/other"),
        ],
        progress: 0.5,
        save_path: "/downloads".into(),
        files: vec![],
        size: 1,
        state: "downloading".into(),
        source: "https://pt.example/other".into(),
    }
}

#[tokio::test]
async fn task_center_keeps_pending_found_by_info_hash() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    h.run_subscribe(id).await;
    let (source, hash) = {
        let t = &h.qb.tasks.lock()[0];
        (t.source.clone(), t.hash.clone())
    };
    let store = h.state.store();
    store
        .lock()
        .set_pending_info_hash(id, &source, &hash)
        .unwrap();
    assert_eq!(store.lock().load_pending_info_hashes(id).unwrap().len(), 1);
    // The user stripped our tags; another owned task keeps tags visible.
    h.qb.tasks.lock()[0].tags.clear();
    h.qb.inject(other_owned_task());
    h.age_pending(id, 16 * 60);
    h.list_tasks().await;
    assert_eq!(
        h.pending_state(id, "active"),
        1,
        "still present by info hash"
    );
}

#[tokio::test]
async fn empty_client_list_does_not_release_http_pending() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    h.run_subscribe(id).await;
    h.qb.remove_all(); // e.g. qB restarted and has not loaded its torrents yet
    h.age_pending(id, 16 * 60);
    h.list_tasks().await;
    assert_eq!(
        h.pending_state(id, "active"),
        1,
        "empty list is no evidence"
    );
    h.transfer(id).await.ok();
    assert_eq!(
        h.pending_state(id, "active"),
        1,
        "background reconcile agrees"
    );
}
