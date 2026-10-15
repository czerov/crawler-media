//! D-H2: the client already has the same torrent (same info hash) without our
//! tags, e.g. added by hand. Submission claims it instead of failing forever.

use super::harness::*;
use super::mock_qbit::Task;

const INFO: &str = "d6:lengthi12e4:name39:The.Matrix.1999.2160p.BluRay.x265-GROUP12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae";

#[tokio::test]
async fn existing_same_infohash_task_is_claimed_and_imported() {
    let h = Harness::new();
    h.create_site_at_mock().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    let torrent = format!("d8:announce17:https://t.ex/anno4:info{INFO}e");
    let hash = downloader::torrent_info_hash(torrent.as_bytes()).unwrap();
    *h.qb.torrent_file.lock() = Some(torrent);
    h.qb.inject(Task {
        hash: hash.clone(),
        name: "user-added".into(),
        tags: vec!["mine".into()],
        progress: 0.4,
        save_path: h.qb.save_root.display().to_string(),
        files: vec![],
        size: 1,
        state: "downloading".into(),
        source: "manual".into(),
    });

    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 0, "no duplicate add (qB would answer Fails.)");
    assert_eq!(h.qb.add_tags.lock().len(), 1, "claimed via addTags");
    let task = h.qb.tasks.lock()[0].clone();
    assert!(task.tags.iter().any(|t| t == "mine"), "user tags kept");
    let enclosure = h.pending(id)[0].1.torrent.enclosure.clone();
    assert!(task.tags.iter().any(|t| t == &downloader::ownership_tag(&enclosure)));
    assert_eq!(h.pending_state(id, "active"), 1);

    let video = probed_video(2160, "hevc", false);
    h.qb.complete(0, MATRIX_TITLE, &[(&format!("{MATRIX_TITLE}.mkv"), &video)]);
    h.transfer(id).await.unwrap();
    assert_eq!(h.library_files().len(), 1, "claimed task imported");
    assert_eq!(h.pending_state(id, "imported"), 1);
}
