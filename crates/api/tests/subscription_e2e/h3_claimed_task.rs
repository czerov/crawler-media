//! Review H-3: a user's existing task claimed by info hash keeps its data:
//! deleting the subscription never deletes the task's files.

use axum::http::StatusCode;
use tower::ServiceExt;

use super::harness::*;
use super::mock_qbit::Task;
use crate::common::request;

const INFO: &str = "d6:lengthi12e4:name39:The.Matrix.1999.2160p.BluRay.x265-GROUP12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae";

#[tokio::test]
async fn deleting_subscription_keeps_claimed_task_files() {
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
    assert_eq!(h.qb.add_count(), 0, "claimed, not added");
    let task = h.qb.tasks.lock()[0].clone();
    assert!(
        task.tags.iter().any(|t| t == downloader::CLAIMED_TAG),
        "claimed tag: {:?}",
        task.tags
    );

    let response = h
        .app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/subscriptions/{id}?delete_torrents=true"),
            Some(TOKEN),
            serde_json::Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let deletes = h.qb.deletes.lock().clone();
    assert!(!deletes.is_empty(), "task removed from client");
    assert!(
        deletes.iter().all(|req| req.contains("deleteFiles=false")),
        "claimed task files must be kept: {deletes:?}"
    );
}
