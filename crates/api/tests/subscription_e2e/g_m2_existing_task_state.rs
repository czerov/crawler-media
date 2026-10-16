//! G-M2: an already-owned qBittorrent task short-circuits the add. A broken
//! one (error / missingFiles) must not count as submitted: the enclosure is
//! rejected and the next round picks another source. A paused one is resumed.

use super::harness::*;
use super::mock_qbit::Task;

fn owned_task(state: &str, enclosure: &str) -> Task {
    Task {
        hash: "ab".repeat(20),
        name: MATRIX_TITLE.into(),
        tags: vec!["crawler-media".into(), downloader::ownership_tag(enclosure)],
        progress: 0.3,
        save_path: "/downloads".into(),
        files: Vec::new(),
        size: 1,
        state: state.into(),
        source: enclosure.into(),
    }
}

async fn broken_existing_task_is_rejected_and_replaced(state: &str) {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    h.qb.inject(owned_task(state, &enclosure(1)));
    h.set_search_rows(&[(MATRIX_TITLE, 1)]);
    h.scheduled_search(id).await.unwrap();
    assert_eq!(
        h.pending_state(id, "active"),
        0,
        "{state}: broken task is not an active pending"
    );
    assert_eq!(
        h.pending_state(id, "rejected"),
        1,
        "{state}: enclosure rejected"
    );

    let other = "The.Matrix.1999.1080p.BluRay.x264-OTHER";
    h.set_search_rows(&[(MATRIX_TITLE, 1), (other, 2)]);
    h.scheduled_search(id).await.unwrap();
    let active: Vec<_> = h
        .pending(id)
        .into_iter()
        .map(|(_, p)| p.torrent.enclosure)
        .collect();
    assert_eq!(
        active,
        vec![enclosure(2)],
        "{state}: next round switches source"
    );
}

#[tokio::test]
async fn error_task_is_rejected() {
    broken_existing_task_is_rejected_and_replaced("error").await;
}

#[tokio::test]
async fn missing_files_task_is_rejected() {
    broken_existing_task_is_rejected_and_replaced("missingFiles").await;
}

#[tokio::test]
async fn paused_task_is_resumed_and_pending_stays_active() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    h.qb.inject(owned_task("pausedDL", &enclosure(1)));
    h.set_search_rows(&[(MATRIX_TITLE, 1)]);
    h.scheduled_search(id).await.unwrap();
    assert_eq!(h.qb.resumes.lock().len(), 1, "paused task resumed");
    assert_eq!(h.pending_state(id, "active"), 1);
}

/// G-M2 follow-up: the manual run path records the broken task as rejected too.
#[tokio::test]
async fn manual_run_rejects_broken_existing_task() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    h.qb.inject(owned_task("missingFiles", &enclosure(1)));
    h.set_search_rows(&[(MATRIX_TITLE, 1)]);
    let _ = h.run_subscribe_raw(id).await;
    assert_eq!(h.pending_state(id, "active"), 0, "not an active pending");
    assert_eq!(h.pending_state(id, "rejected"), 1, "manual run rejects it");
}
