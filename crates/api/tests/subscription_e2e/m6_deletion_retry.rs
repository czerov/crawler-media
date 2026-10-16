//! M6: when the replaced Library file cannot be deleted, the Transfer still
//! commits (facts → new file, pending imported) and queues the deletion; a
//! later Transfer retries it and cleans the old ledger row.

use std::os::unix::fs::PermissionsExt;

use domain::{Confidence, LedgerId, LedgerRow, QualitySource, SubscribeId};

use super::harness::*;

async fn wash_owning_locked_1080p(h: &Harness) -> (SubscribeId, std::path::PathBuf) {
    h.create_site().await;
    let id = h
        .create_movie_subscribe(serde_json::json!({ "wash_cut": true }))
        .await;
    let media_id = h
        .state
        .store()
        .lock()
        .get_subscribe(id)
        .unwrap()
        .unwrap()
        .media_id;
    let owned = h
        .movie_library_root()
        .join("locked")
        .join("The Matrix (1999) - 1080p.mkv");
    std::fs::create_dir_all(owned.parent().unwrap()).unwrap();
    std::fs::write(&owned, probed_video(1080, "h264", false)).unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: owned.display().to_string(),
        season: None,
        episode: None,
        resolution: Some("1080p".into()),
        codec: Some("h264".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(1),
    };
    let mut facts = subscribe::SubscribeFacts::default();
    facts.replace(
        None,
        None,
        subscribe::QualityFact {
            score: 1,
            path: Some(row.path.clone()),
        },
    );
    facts.set_quality(
        row.path.clone(),
        release::parse("The.Matrix.1999.1080p.WEB-DL.x264"),
    );
    let store = h.state.store();
    let store = store.lock();
    store.insert_ledger(&row).unwrap();
    store.save_subscribe_facts(id, &facts).unwrap();
    (id, owned)
}

fn set_mode(dir: &std::path::Path, mode: u32) {
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[tokio::test]
async fn failed_delete_is_queued_and_retried_on_next_transfer() {
    let h = Harness::new();
    let (id, owned) = wash_owning_locked_1080p(&h).await;
    let locked = owned.parent().unwrap().to_path_buf();
    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1);
    let video = probed_video(2160, "hevc", false);
    h.qb.complete(0, MATRIX_TITLE, &[(&format!("{MATRIX_TITLE}.mkv"), &video)]);

    set_mode(&locked, 0o555);
    let result = h.transfer(id).await;
    set_mode(&locked, 0o755);
    let owned_s = owned.display().to_string();
    let fact = h.owned_movie_path(id).unwrap();
    assert_ne!(fact, owned_s, "facts point at the new file: {result:?}");
    assert!(std::path::Path::new(&fact).is_file());
    assert!(owned.is_file(), "old file survived the EACCES");
    assert_eq!(
        h.pending_state(id, "active"),
        0,
        "pending committed as imported"
    );
    let queued = h
        .state
        .store()
        .lock()
        .list_pending_file_deletions()
        .unwrap();
    assert_eq!(queued, vec![owned_s.clone()]);

    // Review M-3: inside the backoff window the row is not retried.
    h.transfer(id).await.unwrap();
    assert!(owned.exists(), "backoff respected");
    // Backoff expired: the next Transfer job finishes the deletion.
    h.state
        .store()
        .lock()
        .reset_file_deletion_backoff(&owned_s)
        .unwrap();
    h.transfer(id).await.unwrap();
    assert!(!owned.exists());
    assert!(!h.ledger_paths().contains(&owned_s));
    assert!(
        h.state
            .store()
            .lock()
            .list_pending_file_deletions()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn queued_deletion_never_removes_a_new_file_at_the_same_path() {
    let h = Harness::new();
    let (id, owned) = wash_owning_locked_1080p(&h).await;
    let locked = owned.parent().unwrap().to_path_buf();
    h.run_subscribe(id).await;
    let video = probed_video(2160, "hevc", false);
    h.qb.complete(0, MATRIX_TITLE, &[(&format!("{MATRIX_TITLE}.mkv"), &video)]);
    set_mode(&locked, 0o555);
    let _ = h.transfer(id).await;
    set_mode(&locked, 0o755);
    let owned_s = owned.display().to_string();
    assert_eq!(
        h.state
            .store()
            .lock()
            .list_pending_file_deletions()
            .unwrap(),
        vec![owned_s.clone()]
    );

    // The user puts a different file at the same path before the retry.
    std::fs::remove_file(&owned).unwrap();
    std::fs::write(&owned, b"a different, user-provided file").unwrap();
    h.state
        .store()
        .lock()
        .reset_file_deletion_backoff(&owned_s)
        .unwrap();
    h.transfer(id).await.unwrap();
    assert!(owned.is_file(), "reused path must not be deleted");
    assert!(
        h.state
            .store()
            .lock()
            .list_pending_file_deletions()
            .unwrap()
            .is_empty(),
        "stale queue row dropped"
    );
}

/// Review L-10: exhausted deletions are listed for the user.
#[tokio::test]
async fn stuck_deletions_are_listed() {
    let h = Harness::new();
    {
        let store = h.state.store();
        let store = store.lock();
        store
            .enqueue_file_deletion("/nowhere/x.mkv", "wash_replaced")
            .unwrap();
        for _ in 0..store::MAX_FILE_DELETION_ATTEMPTS {
            store
                .record_file_deletion_failure("/nowhere/x.mkv", 0, "EACCES")
                .unwrap();
        }
    }
    let response = tower::ServiceExt::oneshot(
        h.app.clone(),
        super::common::request(
            "GET",
            "/api/v1/library/pending-deletions",
            Some(TOKEN),
            serde_json::Value::Null,
        ),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = super::common::json_body(response).await;
    assert_eq!(body["data"]["queued"], 1, "{body}");
    assert_eq!(body["data"]["stuck"][0]["last_error"], "EACCES", "{body}");
}
