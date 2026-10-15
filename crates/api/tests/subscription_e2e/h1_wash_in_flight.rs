//! H1: while one wash-cut upgrade for a slot is in flight, later rounds must
//! not submit more upgrades (same quality, different enclosure) for it.

use super::harness::*;

#[tokio::test]
async fn in_flight_upgrade_blocks_further_upgrades_for_the_same_slot() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({ "wash_cut": true })).await;
    let owned = h.seed_owned_movie(id, 1080, "h264", 1);

    h.set_search_rows(&[("The.Matrix.1999.2160p.BluRay.x265-GROUP", 1)]);
    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1, "first upgrade submitted");
    h.qb.set_progress(0, 0.3);

    h.set_search_rows(&[
        ("The.Matrix.1999.2160p.BluRay.x265-OTHER", 2),
        ("The.Matrix.1999.2160p.BluRay.x265-THIRD", 3),
    ]);
    h.scheduled_search(id).await.unwrap();
    assert_eq!(h.qb.add_count(), 1, "scheduled round: no second upgrade while A is in flight");
    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1, "manual round: no second upgrade while A is in flight");

    let video = probed_video(2160, "hevc", false);
    h.qb.complete(0, MATRIX_TITLE, &[(&format!("{MATRIX_TITLE}.mkv"), &video)]);
    h.transfer(id).await.unwrap();
    assert!(!owned.exists(), "A replaced the owned 1080p");
    let files = h.library_files();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(h.owned_movie_path(id).as_deref(), Some(files[0].display().to_string().as_str()));
}
