//! F-1: with `subscribe.selective_files` on, a range pack chosen only to fill
//! a gap downloads just the missing episodes (qB filePrio 0 / Transmission
//! files-unwanted); the wanted files completing finishes the pending row.

use std::sync::Arc;

use serde_json::json;

use super::fake_catalog::tv_subscribe;
use super::harness::*;
use super::mock_transmission::MockTransmission;

const E01: &str = "The.Long.Watch.S01E01.1080p.WEB-DL.x264-GRP";
const PACK: &str = "The.Long.Watch.S01E01-E03.1080p.WEB-DL.x264-PACK";

fn pack_files() -> Vec<String> {
    (1..=3)
        .map(|e| format!("{PACK}/The.Long.Watch.S01E0{e}.1080p.mkv"))
        .chain([format!("{PACK}/{PACK}.nfo")])
        .collect()
}

fn enable(h: &Harness, on: bool) {
    h.state
        .store()
        .lock()
        .put_setting(
            api::settings_keys::SUBSCRIBE_SELECTIVE_FILES,
            if on { "true" } else { "false" },
        )
        .unwrap();
}

fn window() -> serde_json::Value {
    json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 3 })
}

fn videos(h: &Harness, needle: &str) -> usize {
    h.library_files()
        .into_iter()
        .filter(|p| super::is_video(p) && p.to_string_lossy().contains(needle))
        .count()
}

async fn qbit_run(on: bool) -> (Harness, Vec<String>) {
    let h = Harness::new();
    enable(&h, on);
    h.create_site().await;
    let id = tv_subscribe(&h, window(), json!({})).await;
    let video = probed_video(1080, "h264", false);
    h.set_search_rows(&[(E01, 1)]);
    h.scheduled_search(id).await.unwrap();
    h.qb.complete(0, E01, &[(&format!("{E01}.mkv"), &video)]);
    h.transfer(id).await.unwrap();

    *h.qb.next_files.lock() = pack_files()
        .into_iter()
        .map(|f| format!("{PACK}/{f}"))
        .collect();
    h.set_search_rows(&[(PACK, 2)]);
    h.scheduled_search(id).await.unwrap();
    assert_eq!(h.qb.add_count(), 2, "pack submitted");
    let prios = h.qb.file_prios.lock().clone();
    // The client only finishes what it was asked for.
    h.qb.complete(
        1,
        PACK,
        &[
            (&format!("{PACK}/The.Long.Watch.S01E02.1080p.mkv"), &video),
            (&format!("{PACK}/The.Long.Watch.S01E03.1080p.mkv"), &video),
        ],
    );
    {
        let mut tasks = h.qb.tasks.lock();
        tasks[1].files.insert(
            0,
            (
                format!("{PACK}/{PACK}/The.Long.Watch.S01E01.1080p.mkv"),
                0.0,
            ),
        );
        if on {
            // qB never reaches 100% overall when a file is skipped.
            tasks[1].progress = 0.7;
        }
    }
    h.transfer(id).await.unwrap();
    assert_eq!(videos(&h, "E01"), 1);
    assert_eq!(videos(&h, "E02"), 1);
    assert_eq!(videos(&h, "E03"), 1);
    assert_eq!(h.pending_state(id, "active"), 0, "pack finished");
    (h, prios)
}

#[tokio::test]
async fn qbit_pack_downloads_only_missing_episodes() {
    let (_h, prios) = qbit_run(true).await;
    assert_eq!(prios.len(), 1, "one filePrio call: {prios:?}");
    assert!(prios[0].contains("priority=0"), "{}", prios[0]);
    assert!(
        prios[0].contains("id=0") && !prios[0].contains("id=0%7C") && !prios[0].contains("id=0|"),
        "only E01 (index 0) skipped: {}",
        prios[0]
    );
}

#[tokio::test]
async fn qbit_selective_off_keeps_whole_pack() {
    let (_h, prios) = qbit_run(false).await;
    assert!(prios.is_empty(), "no filePrio when disabled");
}

async fn transmission_run(on: bool) -> Vec<serde_json::Value> {
    let dir = tempfile::tempdir().unwrap();
    let mt = MockTransmission::start(&dir.path().join("tr"), 17);
    let client: Arc<dyn downloader::Downloader> =
        Arc::new(downloader::TransmissionDownloader::connect(mt.config()).unwrap());
    let h = Harness::with_downloader(client);
    enable(&h, on);
    h.create_site().await;
    let id = tv_subscribe(&h, window(), json!({})).await;
    let video = probed_video(1080, "h264", false);
    h.set_search_rows(&[(E01, 1)]);
    h.scheduled_search(id).await.unwrap();
    mt.complete(0, E01, &[(&format!("{E01}.mkv"), &video)]);
    h.transfer(id).await.unwrap();

    *mt.next_files.lock() = pack_files();
    h.set_search_rows(&[(PACK, 2)]);
    h.scheduled_search(id).await.unwrap();
    assert_eq!(mt.tasks.lock().len(), 2, "pack submitted");
    let sets = mt.sets.lock().clone();
    mt.complete(
        1,
        PACK,
        &[
            ("The.Long.Watch.S01E02.1080p.mkv", &video),
            ("The.Long.Watch.S01E03.1080p.mkv", &video),
        ],
    );
    mt.tasks.lock()[1].files.push((
        format!("{PACK}/The.Long.Watch.S01E01.1080p.mkv"),
        10,
        0,
        false,
    ));
    h.transfer(id).await.unwrap();
    assert_eq!(videos(&h, "E02"), 1);
    assert_eq!(videos(&h, "E03"), 1);
    assert_eq!(h.pending_state(id, "active"), 0, "pack finished");
    sets.into_iter()
        .filter(|s| s.get("files-unwanted").is_some())
        .collect()
}

#[tokio::test]
async fn transmission_pack_downloads_only_missing_episodes() {
    let sets = transmission_run(true).await;
    assert_eq!(sets.len(), 1, "{sets:?}");
    assert_eq!(sets[0]["files-unwanted"], json!([0]));
    assert_eq!(sets[0]["ids"], json!([2]));
}

#[tokio::test]
async fn transmission_selective_off_keeps_whole_pack() {
    assert!(transmission_run(false).await.is_empty());
}

/// F-1 follow-up: a magnet/HTTP task whose file list is not known at submit
/// time is filtered later by the Transfer-time reconcile, exactly once.
#[tokio::test]
async fn qbit_filters_files_once_metadata_arrives() {
    let h = Harness::new();
    enable(&h, true);
    h.create_site().await;
    let id = tv_subscribe(&h, window(), json!({})).await;
    let video = probed_video(1080, "h264", false);
    h.set_search_rows(&[(E01, 1)]);
    h.scheduled_search(id).await.unwrap();
    h.qb.complete(0, E01, &[(&format!("{E01}.mkv"), &video)]);
    h.transfer(id).await.unwrap();

    h.set_search_rows(&[(PACK, 2)]);
    h.scheduled_search(id).await.unwrap(); // no metadata yet
    assert!(h.qb.file_prios.lock().is_empty());
    h.qb.tasks.lock()[1].files = pack_files().into_iter().map(|f| (f, 0.0)).collect();
    h.transfer(id).await.unwrap();
    h.transfer(id).await.unwrap();
    let prios = h.qb.file_prios.lock().clone();
    assert_eq!(prios.len(), 1, "filtered once after metadata: {prios:?}");
    assert!(prios[0].contains("id=0&"), "{}", prios[0]);
}
