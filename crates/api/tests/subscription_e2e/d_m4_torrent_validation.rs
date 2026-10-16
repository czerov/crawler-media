//! D-M4: a site answering the .torrent link with an HTML page must not be
//! uploaded as a torrent; the temp .torrent file never outlives the add.

use super::harness::*;

const INFO: &str = "d6:lengthi12e4:name39:The.Matrix.1999.2160p.BluRay.x265-GROUP12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae";

fn temp_torrents() -> usize {
    let prefix = format!("cm-torrent-{}-", std::process::id());
    std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .count()
}

async fn submit_with(body: &str) -> String {
    let h = Harness::new();
    h.create_site_at_mock().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    *h.qb.torrent_file.lock() = Some(body.to_string());
    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1);
    h.qb.tasks.lock()[0].source.clone()
}

#[tokio::test]
async fn html_login_page_is_not_uploaded_as_torrent() {
    let source = submit_with("<!DOCTYPE html><html><body>login</body></html>").await;
    assert_ne!(source, "file", "HTML not uploaded as a .torrent");
}

#[tokio::test]
async fn valid_torrent_is_uploaded_and_temp_file_removed() {
    let source = submit_with(&format!("d8:announce17:https://t.ex/anno4:info{INFO}e")).await;
    assert_eq!(source, "file");
    assert_eq!(temp_torrents(), 0, "no cm-torrent-* temp files left");
}
