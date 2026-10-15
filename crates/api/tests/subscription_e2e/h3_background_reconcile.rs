//! H3: pending rows whose task vanished from the client are reconciled by the
//! background Transfer job (no UI request needed), so the slot is released.

use super::harness::*;

#[tokio::test]
async fn transfer_job_cleans_vanished_task_and_next_search_resubmits() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    h.set_search_rows(&[(MATRIX_TITLE, 1)]);
    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1);

    h.qb.remove_all(); // user deleted the task in qB
    h.age_pending(id, 5 * 60);
    h.transfer(id).await.unwrap();
    assert_eq!(h.pending_state(id, "active"), 1, "inside grace period: keep");

    h.age_pending(id, 16 * 60);
    h.transfer(id).await.unwrap();
    assert_eq!(h.pending_state(id, "active"), 0, "background Transfer reconciled the vanished task");

    h.set_search_rows(&[("The.Matrix.1999.2160p.BluRay.x265-OTHER", 2)]);
    h.scheduled_search(id).await.unwrap();
    assert_eq!(h.qb.add_count(), 2, "slot released: another source is submitted");
    assert_eq!(h.pending_state(id, "active"), 1);
}

#[tokio::test]
async fn transfer_job_keeps_in_flight_task() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    h.run_subscribe(id).await;
    h.qb.set_progress(0, 0.3);
    h.age_pending(id, 16 * 60);
    h.transfer(id).await.unwrap();
    assert_eq!(h.pending_state(id, "active"), 1);
    h.scheduled_search(id).await.unwrap();
    assert_eq!(h.qb.add_count(), 1);
}
