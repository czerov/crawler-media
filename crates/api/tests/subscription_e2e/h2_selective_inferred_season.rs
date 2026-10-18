//! Review H-2: in a season-2 pack, files without a season marker inherit the
//! torrent's season (not the parser's default S01), so needed episodes are
//! not deselected.

use serde_json::json;

use super::fake_catalog::tv_subscribe;
use super::harness::*;

const E01: &str = "The.Long.Watch.S02E01.1080p.WEB-DL.x264-GRP";
const PACK: &str = "The.Long.Watch.S02E01-E03.1080p.WEB-DL.x264-PACK";

#[tokio::test]
async fn season_less_files_in_s02_pack_keep_missing_episodes() {
    let h = Harness::new();
    h.state
        .store()
        .lock()
        .put_setting(api::settings_keys::SUBSCRIBE_SELECTIVE_FILES, "true")
        .unwrap();
    h.create_site().await;
    let window = json!({ "kind": "tv", "season": 2, "episode_from": 1, "episode_to": 3 });
    let id = tv_subscribe(&h, window, json!({})).await;
    let video = probed_video(1080, "h264", false);
    h.set_search_rows(&[(E01, 1)]);
    h.scheduled_search(id).await.unwrap();
    h.qb.complete(0, E01, &[(&format!("{E01}.mkv"), &video)]);
    h.transfer(id).await.unwrap();

    *h.qb.next_files.lock() = (1..=3)
        .map(|e| format!("{PACK}/The Long Watch - E0{e}.mkv"))
        .chain([format!("{PACK}/info.nfo")])
        .collect();
    h.set_search_rows(&[(PACK, 2)]);
    h.scheduled_search(id).await.unwrap();
    assert_eq!(h.qb.add_count(), 2, "pack submitted");
    let skipped: Vec<usize> = h.qb.skipped.lock().iter().map(|(_, i)| *i).collect();
    assert_eq!(skipped, vec![0], "only the owned E01 is skipped");
}
