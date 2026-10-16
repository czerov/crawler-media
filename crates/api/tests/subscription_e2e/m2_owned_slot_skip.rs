//! M2: without wash-cut, a range pack that also covers an owned episode must
//! only transfer the missing episode; the owned file and its fact stay put.

use serde_json::json;

use super::fake_catalog::tv_subscribe;
use super::harness::*;

const E01: &str = "The.Long.Watch.S01E01.1080p.WEB-DL.x264-GRP";
const PACK: &str = "The.Long.Watch.S01E01-E02.1080p.WEB-DL.x264-PACK";

fn video_files(h: &Harness, needle: &str) -> Vec<std::path::PathBuf> {
    h.library_files()
        .into_iter()
        .filter(|p| super::is_video(p) && p.to_string_lossy().contains(needle))
        .collect()
}

#[tokio::test]
async fn pack_over_owned_episode_transfers_only_the_missing_one() {
    let h = Harness::new();
    h.create_site().await;
    let window = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 });
    let id = tv_subscribe(&h, window, json!({})).await;
    let video = probed_video(1080, "h264", false);

    h.set_search_rows(&[(E01, 1)]);
    h.scheduled_search(id).await.unwrap();
    h.qb.complete(0, E01, &[(&format!("{E01}.mkv"), &video)]);
    h.transfer(id).await.unwrap();
    let owned = video_files(&h, "E01");
    assert_eq!(owned.len(), 1, "E01 imported once");
    let facts_before = h.state.store().lock().load_subscribe_facts(id).unwrap();
    let e01_before = facts_before.get(Some(1), Some(1)).cloned();

    h.set_search_rows(&[(PACK, 2)]);
    h.scheduled_search(id).await.unwrap();
    assert_eq!(h.qb.add_count(), 2, "pack submitted for missing E02");
    h.qb.complete(
        1,
        PACK,
        &[
            (&format!("{PACK}/The.Long.Watch.S01E01.1080p.mkv"), &video),
            (&format!("{PACK}/The.Long.Watch.S01E02.1080p.mkv"), &video),
        ],
    );
    h.transfer(id).await.unwrap();

    assert_eq!(
        video_files(&h, "E01"),
        owned,
        "owned E01 untouched, no duplicate"
    );
    assert_eq!(video_files(&h, "E02").len(), 1, "E02 imported");
    let facts = h.state.store().lock().load_subscribe_facts(id).unwrap();
    assert_eq!(
        facts.get(Some(1), Some(1)).cloned(),
        e01_before,
        "E01 fact unchanged"
    );
    let e01_ledger = h
        .ledger_paths()
        .into_iter()
        .filter(|p| p.contains("E01"))
        .count();
    assert_eq!(e01_ledger, 1, "no new ledger row for E01");
}
