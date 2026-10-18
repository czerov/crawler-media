//! Template scenario: create subscription → search → submit to (mock) qB →
//! download completes → Transfer imports into the Library.

use super::harness::*;

#[tokio::test]
async fn movie_subscription_flows_from_search_to_library() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;

    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1, "exactly one submission");
    let task = h.qb.tasks.lock()[0].clone();
    assert!(task.tags.iter().any(|t| t == "crawler-media"));
    assert!(
        task.tags
            .iter()
            .any(|t| t == &downloader::ownership_tag(MATRIX_ENCLOSURE))
    );
    assert_eq!(h.pending(id).len(), 1);

    // Still downloading: Transfer imports nothing and keeps the pending row.
    h.transfer(id).await.unwrap();
    assert!(h.library_files().is_empty());
    assert_eq!(h.pending_state(id, "active"), 1);

    let video = probed_video(2160, "hevc", false);
    h.qb.complete(0, MATRIX_TITLE, &[(&format!("{MATRIX_TITLE}.mkv"), &video)]);
    h.transfer(id).await.unwrap();

    let files = h.library_files();
    assert_eq!(files.len(), 1, "imported: {files:?}");
    assert_eq!(std::fs::read(&files[0]).unwrap(), video);
    assert_eq!(h.ledger_paths().len(), 1);
    let row = h.state.store().lock().list_ledger().unwrap().remove(0);
    assert_eq!(
        row.resolution.as_deref(),
        Some("2160p"),
        "quality came from the fake ffprobe"
    );
    assert_eq!(row.quality_source, domain::QualitySource::Probe);
    assert_eq!(h.pending_state(id, "active"), 0);
    assert_eq!(h.pending_state(id, "imported"), 1);

    // A second run must not submit the owned movie again.
    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1);
}
