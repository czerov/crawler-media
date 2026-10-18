//! H2: a wash-cut download whose probe disproves the upgrade must not be
//! chosen, submitted and rejected again on every later round.

use super::harness::*;

#[tokio::test]
async fn probe_rejected_upgrade_is_not_resubmitted() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({ "wash_cut": true })).await;
    let owned = h.seed_owned_movie(id, 1080, "h264", 1);

    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1);
    // Title says 2160p, the file is really 720p: a downgrade, vetoed by probe.
    let fake = probed_video(720, "h264", false);
    h.qb.complete(0, MATRIX_TITLE, &[(&format!("{MATRIX_TITLE}.mkv"), &fake)]);
    h.transfer(id).await.unwrap();
    assert!(owned.is_file(), "rejected arrival keeps the owned file");
    assert_eq!(h.owned_movie_path(id).as_deref(), Some(owned.display().to_string().as_str()));
    assert_eq!(h.pending_state(id, "rejected"), 1, "probe veto is recorded as rejected");

    // The client no longer has the task (user cleaned it up).
    h.qb.remove_all();
    for _ in 0..3 {
        h.scheduled_search(id).await.unwrap();
    }
    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1, "rejected enclosure is never resubmitted automatically");
    assert_eq!(h.pending_state(id, "rejected"), 1);

    // An explicit user admit of the same enclosure is honoured and revives it.
    let response = tower::ServiceExt::oneshot(
        h.app.clone(),
        super::common::request(
            "POST",
            "/api/v1/search/admit",
            Some(TOKEN),
            serde_json::json!({ "subscribe_id": id.to_string(), "enclosure": MATRIX_ENCLOSURE }),
        ),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(h.qb.add_count(), 2, "manual admit submits");
    assert_eq!(h.pending_state(id, "active"), 1, "manual admit revives the row");
    assert_eq!(h.pending_state(id, "rejected"), 0);
}
