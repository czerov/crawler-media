//! D-S1: the task-center absent cleanup must recognise HTTP-enclosure tasks
//! by their per-torrent ownership tag and must never delete an in-flight row.

use super::harness::*;
use super::mock_qbit::Task;

#[tokio::test]
async fn task_listing_keeps_in_flight_http_pending_past_grace_period() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1);
    h.qb.set_progress(0, 0.3);

    // 16 minutes later the user opens the task center.
    h.age_pending(id, 16 * 60);
    let listed = h.list_tasks().await;
    let items = listed["data"]["items"].as_array().cloned().unwrap_or_default();
    assert_eq!(h.pending_state(id, "active"), 1, "in-flight pending must survive: {listed}");
    assert_eq!(items.len(), 1, "{listed}");
    assert_eq!(items[0]["state"], "downloading", "{listed}");
    assert_eq!(
        items[0]["info_hash"].as_str(),
        Some(h.qb.tasks.lock()[0].hash.as_str())
    );

    // Download finishes and is imported; no duplicate submission happened.
    let video = probed_video(2160, "hevc", false);
    h.qb.complete(0, MATRIX_TITLE, &[(&format!("{MATRIX_TITLE}.mkv"), &video)]);
    h.transfer(id).await.unwrap();
    assert_eq!(h.library_files().len(), 1);
    assert_eq!(h.pending_state(id, "imported"), 1);
    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1);
}

#[tokio::test]
async fn task_listing_still_cleans_a_task_removed_from_the_client() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    h.run_subscribe(id).await;
    // Another, unrelated owned task keeps identity visible for this client.
    h.qb.inject(Task {
        hash: "f".repeat(40),
        name: "Other.Show.S01E01".into(),
        tags: vec!["crawler-media".into(), downloader::ownership_tag("https://pt.example/other")],
        progress: 0.5,
        save_path: "/downloads".into(),
        files: vec![],
        size: 1,
        state: "downloading".into(),
        source: "https://pt.example/other".into(),
    });
    h.qb.tasks.lock().remove(0); // user deleted our task in qB

    h.age_pending(id, 5 * 60);
    h.list_tasks().await;
    assert_eq!(h.pending_state(id, "active"), 1, "inside grace period: keep");

    h.age_pending(id, 16 * 60);
    let listed = h.list_tasks().await;
    assert_eq!(h.pending_state(id, "active"), 0, "absent past grace: cleaned {listed}");
}

#[tokio::test]
async fn task_listing_refuses_cleanup_when_client_hides_ownership_tags() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    h.run_subscribe(id).await;
    // Client lost per-torrent tags (only the generic tag survives).
    for task in h.qb.tasks.lock().iter_mut() {
        task.tags = vec!["crawler-media".into()];
    }
    h.age_pending(id, 16 * 60);
    h.list_tasks().await;
    assert_eq!(h.pending_state(id, "active"), 1, "unprovable identity must not delete pending");
}
