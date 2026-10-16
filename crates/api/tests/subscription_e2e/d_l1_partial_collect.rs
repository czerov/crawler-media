//! D-L1: finished files of a season pack are imported while other files are
//! still downloading; the pending row is only imported once all are done.

use serde_json::json;

use super::fake_catalog::tv_subscribe;
use super::harness::*;

const PACK: &str = "The.Long.Watch.S01E01-E02.1080p.WEB-DL.x264-GRP";

fn imported(h: &Harness) -> Vec<String> {
    h.library_files()
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "mkv"))
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

#[tokio::test]
async fn first_episode_imports_before_the_pack_finishes() {
    let h = Harness::new();
    h.create_site().await;
    let window = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 });
    let id = tv_subscribe(&h, window, json!({})).await;
    h.set_search_rows(&[(PACK, 1)]);
    h.scheduled_search(id).await.unwrap();
    let video = probed_video(1080, "h264", false);
    let e01 = "The.Long.Watch.S01E01.1080p.WEB-DL.x264-GRP.mkv";
    let e02 = "The.Long.Watch.S01E02.1080p.WEB-DL.x264-GRP.mkv";
    h.qb.complete(0, PACK, &[(e01, &video), (e02, &video)]);
    h.qb.set_file_progress(0, 1, 0.5);

    h.transfer(id).await.unwrap();
    let first = imported(&h);
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(first[0].contains("E01"));
    assert_eq!(h.pending_state(id, "imported"), 0, "pack still downloading");

    h.qb.set_file_progress(0, 1, 1.0);
    h.transfer(id).await.unwrap();
    assert_eq!(imported(&h).len(), 2);
    assert_eq!(h.pending_state(id, "imported"), 1);
}
