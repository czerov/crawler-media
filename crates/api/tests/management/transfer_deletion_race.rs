//! Concurrency tests proving transfer and deletion serialization.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use api::router;
use axum::http::StatusCode;
use domain::Torrent;
use downloader::{Downloader, DownloaderError};
use parking_lot::Mutex;
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tower::ServiceExt;

use super::common::{Fixtures, json_data, request, state, subscribe_payload};

struct GatedDownloader {
    stage_dir: PathBuf,
    started_tx: Mutex<Option<mpsc::UnboundedSender<()>>>,
    resume_rx: Mutex<Option<oneshot::Receiver<()>>>,
    completed_called: Arc<AtomicBool>,
}

impl Downloader for GatedDownloader {
    fn add(&self, _torrent: &Torrent) -> Result<(), DownloaderError> {
        Ok(())
    }

    fn remove(&self, _torrent: &Torrent, _delete_files: bool) -> Result<(), DownloaderError> {
        Ok(())
    }

    fn delete_task(&self, _info_hash: &str, _delete_files: bool) -> Result<(), DownloaderError> {
        Ok(())
    }

    fn remove_owned(&self, _torrent: &Torrent, _delete_files: bool) -> Result<(), DownloaderError> {
        Ok(())
    }

    fn endpoint(&self) -> Option<String> {
        Some("memory|gated".into())
    }

    fn completed_files(&self, _torrent: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        self.completed_called.store(true, Ordering::SeqCst);
        if let Some(tx) = self.started_tx.lock().take() {
            let _ = tx.send(());
        }
        let rx = self.resume_rx.lock().take();
        if let Some(rx) = rx {
            let _ = rx.blocking_recv();
        }
        let file_path = self.stage_dir.join("The.Matrix.1999.1080p.mkv");
        Ok(vec![file_path])
    }
}

fn make_torrent() -> Torrent {
    Torrent {
        id: None,
        site_id: domain::SiteId::new(),
        title: "The.Matrix.1999.1080p.mkv".into(),
        enclosure: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
        size_bytes: Some(1024),
        seeders: Some(10),
        free: false,
        hr: false,
        imdb_id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    }
}

#[tokio::test]
async fn transfer_started_first_is_finished_before_delete_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let stage_dir = tmp.path().join("stage");
    std::fs::create_dir_all(&stage_dir).unwrap();
    let src_file = stage_dir.join("The.Matrix.1999.1080p.mkv");
    std::fs::write(&src_file, b"video content").unwrap();

    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let (resume_tx, resume_rx) = oneshot::channel();
    let completed_called = Arc::new(AtomicBool::new(false));

    let downloader = Arc::new(GatedDownloader {
        stage_dir: stage_dir.clone(),
        started_tx: Mutex::new(Some(started_tx)),
        resume_rx: Mutex::new(Some(resume_rx)),
        completed_called: completed_called.clone(),
    });

    let api_state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader,
    );
    let app = router(api_state.clone());

    // 1. Create subscription
    let mut sub_body = subscribe_payload("search");
    sub_body["media"] = json!({
        "kind": "movie",
        "title": "The Matrix",
        "year": 1999
    });
    let resp = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            sub_body,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let sub = json_data(resp).await;
    let sub_id: domain::SubscribeId = sub["id"].as_str().unwrap().parse().unwrap();
    let media_id: domain::MediaId = sub["media"]["id"].as_str().unwrap().parse().unwrap();

    // Insert pending download
    let torrent = make_torrent();
    let pending_item = api::store::PendingDownload {
        submitted_at: Some(1000),
        torrent,
        release_override: None,
        downloader_id: None,
    };
    {
        let store = api_state.store();
        store
            .lock()
            .record_pending_submission(sub_id, 100, &pending_item)
            .unwrap();
    }

    // Spawn transfer in background
    let state_for_transfer = api_state.clone();
    let transfer_handle =
        tokio::task::spawn_blocking(move || api::worker::transfer_one(&state_for_transfer, sub_id));

    // Wait until transfer enters completed_files (IO seam)
    tokio::time::timeout(std::time::Duration::from_secs(5), started_rx.recv())
        .await
        .expect("transfer should enter completed_files")
        .expect("channel valid");

    // Spawn DELETE request while transfer is blocked
    let app_for_delete = app.clone();
    let mut delete_handle = tokio::spawn(async move {
        app_for_delete
            .oneshot(request(
                "DELETE",
                &format!(
                    "/api/v1/subscriptions/{sub_id}?delete_torrents=true&delete_library_files=true"
                ),
                Some("management-secret"),
                serde_json::Value::Null,
            ))
            .await
            .unwrap()
    });

    // DELETE should block because transfer holds subscribe_guard
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(150), &mut delete_handle)
            .await
            .is_err(),
        "DELETE must wait for transfer guard"
    );

    // Resume transfer IO
    resume_tx.send(()).unwrap();

    // Both should now complete
    let transfer_res = transfer_handle.await.unwrap();
    assert!(
        transfer_res.is_ok(),
        "transfer should succeed: {:?}",
        transfer_res
    );

    let delete_resp = delete_handle.await.unwrap();
    assert_eq!(delete_resp.status(), StatusCode::OK);

    // Verify deletion cleaned up everything (no orphan facts, ledger, or subscribe)
    let store_arc = api_state.store();
    let store = store_arc.lock();
    assert!(store.get_subscribe(sub_id).unwrap().is_none());
    assert!(store.ledger_for_media(media_id).unwrap().is_empty());
    assert!(
        store
            .load_subscribe_facts(sub_id)
            .unwrap()
            .entries()
            .next()
            .is_none()
    );
}

#[tokio::test]
async fn delete_reserved_first_prevents_transfer_mutation() {
    let tmp = tempfile::tempdir().unwrap();
    let stage_dir = tmp.path().join("stage");
    std::fs::create_dir_all(&stage_dir).unwrap();
    let src_file = stage_dir.join("The.Matrix.1999.1080p.mkv");
    std::fs::write(&src_file, b"video content").unwrap();

    let downloader = Arc::new(GatedDownloader {
        stage_dir: stage_dir.clone(),
        started_tx: Mutex::new(None),
        resume_rx: Mutex::new(None),
        completed_called: Arc::new(AtomicBool::new(false)),
    });

    let api_state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader.clone(),
    );
    let app = router(api_state.clone());

    // 1. Create subscription
    let mut sub_body = subscribe_payload("search");
    sub_body["media"] = json!({
        "kind": "movie",
        "title": "The Matrix",
        "year": 1999
    });
    let resp = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            sub_body,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let sub = json_data(resp).await;
    let sub_id: domain::SubscribeId = sub["id"].as_str().unwrap().parse().unwrap();
    let media_id: domain::MediaId = sub["media"]["id"].as_str().unwrap().parse().unwrap();

    // Mark subscription as deleting (simulating reservation)
    api_state.mark_subscribe_deleting(sub_id);

    // Now run transfer_one
    let state_for_transfer = api_state.clone();
    let res =
        tokio::task::spawn_blocking(move || api::worker::transfer_one(&state_for_transfer, sub_id))
            .await
            .unwrap();

    assert!(res.is_ok());
    // Should NOT have called completed_files or mutated store
    assert!(!downloader.completed_called.load(Ordering::SeqCst));
    let store_arc = api_state.store();
    let store = store_arc.lock();
    assert!(store.ledger_for_media(media_id).unwrap().is_empty());
}

#[tokio::test]
async fn library_freeze_while_downloader_is_blocked_prevents_physical_transfer() {
    let tmp = tempfile::tempdir().unwrap();
    let stage_dir = tmp.path().join("stage");
    std::fs::create_dir_all(&stage_dir).unwrap();
    let src_file = stage_dir.join("The.Matrix.1999.1080p.mkv");
    std::fs::write(&src_file, b"video content").unwrap();
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let (resume_tx, resume_rx) = oneshot::channel();
    let downloader = Arc::new(GatedDownloader {
        stage_dir,
        started_tx: Mutex::new(Some(started_tx)),
        resume_rx: Mutex::new(Some(resume_rx)),
        completed_called: Arc::new(AtomicBool::new(false)),
    });
    let api_state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader,
    );
    let app = router(api_state.clone());
    let mut body = subscribe_payload("search");
    body["media"] = json!({"kind": "movie", "title": "The Matrix", "year": 1999});
    let response = app
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let sub = json_data(response).await;
    let id = sub["id"].as_str().unwrap().parse().unwrap();
    let library = api_state
        .store()
        .lock()
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .unwrap();
    api_state
        .store()
        .lock()
        .record_pending_submission(
            id,
            100,
            &api::store::PendingDownload {
                torrent: make_torrent(),
                submitted_at: Some(1000),
                release_override: None,
                downloader_id: None,
            },
        )
        .unwrap();
    let runner = api_state.clone();
    let transfer = tokio::task::spawn_blocking(move || api::worker::transfer_one(&runner, id));
    tokio::time::timeout(std::time::Duration::from_secs(5), started_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let other_root = tmp.path().join("other-library");
    api_state
        .store()
        .lock()
        .create_library(
            domain::MediaKind::Movie,
            "other",
            &[other_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let removal = api::library_lifecycle::start_library_removal(&api_state, &library.id).unwrap();
    assert_ne!(removal.state, "succeeded");
    resume_tx.send(()).unwrap();
    assert!(transfer.await.unwrap().is_err());
    assert_eq!(std::fs::read(src_file).unwrap(), b"video content");
    assert!(api_state.store().lock().list_ledger().unwrap().is_empty());
    for root in library.root_paths {
        if root.exists() {
            assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
        }
    }
}
