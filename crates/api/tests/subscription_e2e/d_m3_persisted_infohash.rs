//! D-M3: the info hash of a submitted torrent is stored on the pending row, so
//! a Transmission without labels (rpc < 16) can still collect it after the
//! process (and its in-memory identity map) restarted.

use std::sync::Arc;

use super::harness::*;
use super::mock_transmission::MockTransmission;

fn transmission(mt: &MockTransmission) -> Arc<dyn downloader::Downloader> {
    Arc::new(downloader::TransmissionDownloader::connect(mt.config()).unwrap())
}

#[tokio::test]
async fn unlabeled_transmission_collects_after_restart_via_stored_hash() {
    let dir = tempfile::tempdir().unwrap();
    let mt = MockTransmission::start(&dir.path().join("tr"), 15);
    let mut h = Harness::with_downloader(transmission(&mt));
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    h.set_search_rows(&[(MATRIX_TITLE, 1)]);
    h.scheduled_search(id).await.unwrap();
    assert_eq!(mt.tasks.lock().len(), 1);
    let hash = mt.tasks.lock()[0].hash.clone();
    let stored = h.state.store().lock().load_pending_info_hashes(id).unwrap();
    assert_eq!(
        stored.get(&enclosure(1)),
        Some(&hash),
        "hash persisted on pending"
    );

    h.restart(transmission(&mt));
    let video = probed_video(2160, "hevc", false);
    mt.complete(0, MATRIX_TITLE, &[(&format!("{MATRIX_TITLE}.mkv"), &video)]);
    h.transfer(id).await.unwrap();
    assert_eq!(h.library_files().len(), 1, "collected after restart");
    assert_eq!(h.pending_state(id, "imported"), 1);
}
