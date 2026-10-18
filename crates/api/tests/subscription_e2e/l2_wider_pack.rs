//! L2: a pack wider than the window is accepted; only the window is imported.

use serde_json::json;

use super::fake_catalog::tv_subscribe;
use super::harness::*;

const PACK: &str = "The.Long.Watch.S01E01-E13.1080p.WEB-DL.x264-GRP";

#[tokio::test]
async fn wider_pack_imports_only_the_window() {
    let h = Harness::new();
    h.create_site().await;
    let window = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 12 });
    let id = tv_subscribe(&h, window, json!({ "full_season_pack": true })).await;
    h.set_search_rows(&[(PACK, 1)]);
    h.scheduled_search(id).await.unwrap();
    assert_eq!(h.pending(id).len(), 1, "pack accepted");
    let video = probed_video(1080, "h264", false);
    let names: Vec<String> = (1..=13)
        .map(|e| format!("The.Long.Watch.S01E{e:02}.1080p.WEB-DL.x264-GRP.mkv"))
        .collect();
    let files: Vec<(&str, &[u8])> = names
        .iter()
        .map(|n| (n.as_str(), video.as_slice()))
        .collect();
    h.qb.complete(0, PACK, &files);
    h.transfer(id).await.unwrap();
    let imported: Vec<String> = h
        .library_files()
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "mkv"))
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    assert_eq!(imported.len(), 12, "{imported:?}");
    assert!(!imported.iter().any(|p| p.contains("E13")), "{imported:?}");
}
