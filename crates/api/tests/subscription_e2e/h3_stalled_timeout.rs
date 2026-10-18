//! H3 (stalled): a pending download with no progress for longer than the
//! configured threshold is marked `stalled`, its placeholder is released so the
//! next round picks another source, and the downloader task is never deleted.

use super::harness::*;

const DAY: i64 = 86_400;
const OTHER: &str = "The.Matrix.1999.2160p.BluRay.x265-OTHER";

#[tokio::test]
async fn no_progress_past_threshold_marks_stalled_and_switches_source() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    h.set_search_rows(&[(MATRIX_TITLE, 1)]);
    h.scheduled_search(id).await.unwrap();
    assert_eq!(h.qb.add_count(), 1);
    h.age_pending(id, 8 * DAY);

    h.transfer(id).await.unwrap();
    assert_eq!(
        h.pending_state(id, "stalled"),
        1,
        "no progress for 8 days > 7 day default"
    );
    assert_eq!(h.pending_state(id, "active"), 0, "placeholder released");
    assert!(
        h.qb.deletes.lock().is_empty(),
        "downloader task is never deleted"
    );

    h.set_search_rows(&[(MATRIX_TITLE, 1), (OTHER, 2)]);
    h.scheduled_search(id).await.unwrap();
    let active: Vec<_> = h
        .pending(id)
        .into_iter()
        .map(|(_, p)| p.torrent.enclosure)
        .collect();
    assert_eq!(
        active,
        vec![enclosure(2)],
        "stalled source is not picked again"
    );
}

#[tokio::test]
async fn observed_progress_resets_the_stall_timer() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    h.set_search_rows(&[(MATRIX_TITLE, 1)]);
    h.scheduled_search(id).await.unwrap();
    h.age_pending(id, 8 * DAY);
    h.qb.set_progress(0, 0.2);
    h.transfer(id).await.unwrap();
    assert_eq!(
        h.pending_state(id, "active"),
        1,
        "progress seen now: not stalled"
    );
}

#[tokio::test]
async fn threshold_is_configurable() {
    let h = Harness::new();
    h.state
        .store()
        .lock()
        .put_setting(api::settings_keys::SUBSCRIBE_STALLED_TIMEOUT_DAYS, "1")
        .unwrap();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    h.set_search_rows(&[(MATRIX_TITLE, 1)]);
    h.scheduled_search(id).await.unwrap();
    h.age_pending(id, 2 * DAY);
    h.transfer(id).await.unwrap();
    assert_eq!(h.pending_state(id, "stalled"), 1);
}
