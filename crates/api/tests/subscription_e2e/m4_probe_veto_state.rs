//! Review M-4: an S1 veto caused by an unfinished probe keeps the pending row
//! active so a later probe can decide; only a completed non-upgrade probe
//! rejects it, and then the side-by-side file is rolled back.

use super::harness::*;

#[tokio::test]
async fn unprobed_arrival_stays_active_and_is_reevaluated() {
    let h = Harness::new();
    h.create_site().await;
    let id = h
        .create_movie_subscribe(serde_json::json!({ "wash_cut": true }))
        .await;
    let owned = h.seed_owned_movie(id, 1080, "h264", 1);
    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1);
    h.qb.complete(
        0,
        MATRIX_TITLE,
        &[(&format!("{MATRIX_TITLE}.mkv"), &unprobeable_video())],
    );
    h.transfer(id).await.unwrap();
    assert!(owned.is_file());
    assert_eq!(
        h.pending_state(id, "rejected"),
        0,
        "probe not finished: not rejected"
    );
    assert_eq!(h.pending_state(id, "active"), 1, "awaiting a probe");
    assert_eq!(
        h.library_files(),
        vec![owned.clone()],
        "no side-by-side copy kept"
    );

    // The probe now succeeds (e.g. ffprobe no longer times out).
    let video = probed_video(2160, "hevc", false);
    h.qb.complete(0, MATRIX_TITLE, &[(&format!("{MATRIX_TITLE}.mkv"), &video)]);
    h.transfer(id).await.unwrap();
    assert!(
        !owned.exists(),
        "re-evaluated upgrade replaces the owned file"
    );
    let files = h.library_files();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(std::fs::read(&files[0]).unwrap(), video);
    assert_eq!(h.pending_state(id, "imported"), 1);
}

#[tokio::test]
async fn probed_downgrade_is_rejected_and_rolled_back() {
    let h = Harness::new();
    h.create_site().await;
    let id = h
        .create_movie_subscribe(serde_json::json!({ "wash_cut": true }))
        .await;
    let owned = h.seed_owned_movie(id, 1080, "h264", 1);
    h.run_subscribe(id).await;
    let fake = probed_video(720, "h264", false);
    h.qb.complete(0, MATRIX_TITLE, &[(&format!("{MATRIX_TITLE}.mkv"), &fake)]);
    h.transfer(id).await.unwrap();
    assert_eq!(h.pending_state(id, "rejected"), 1);
    assert_eq!(
        h.library_files(),
        vec![owned.clone()],
        "side-by-side file rolled back"
    );
    assert_eq!(h.ledger_paths(), vec![owned.display().to_string()]);
}
