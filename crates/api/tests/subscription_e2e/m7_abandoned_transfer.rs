//! M7: a Transfer abandoned because the subscription was marked for deletion
//! mid-collect rolls back the library files it created in that round.

use super::harness::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deletion_during_collect_rolls_back_new_library_files() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    h.set_search_rows(&[(MATRIX_TITLE, 1)]);
    h.scheduled_search(id).await.unwrap();
    // The fake ffprobe sleeps on files containing SLOWPROBE.
    let video = String::from_utf8(probed_video(2160, "hevc", false))
        .unwrap()
        .replacen("{\"streams\"", "{\"slow\":\"SLOWPROBE\",\"streams\"", 1)
        .into_bytes();
    let sources =
        h.qb.complete(0, MATRIX_TITLE, &[(&format!("{MATRIX_TITLE}.mkv"), &video)]);

    let state = h.state.clone();
    let marker = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        state.mark_subscribe_deleting(id);
    });
    let _ = h.transfer(id).await;
    marker.await.unwrap();

    let leftovers: Vec<_> = h
        .library_files()
        .into_iter()
        .filter(|p| super::is_video(p))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no orphan library file: {leftovers:?}"
    );
    assert!(sources[0].exists(), "download source kept");
    assert!(h.ledger_paths().is_empty(), "no ledger rows");
}
