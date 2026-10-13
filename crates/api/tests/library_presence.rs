mod support;

use axum::http::StatusCode;
use domain::MediaKind;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn restored_missing_row_is_not_pruned() {
    let fix = PresenceFixture::new();
    let (media_id, row) = fix.insert_media_and_ledger("one.mkv");

    // File missing on disk (not created)
    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["data"]["missing"], 1);

    // Now restore file on disk
    std::fs::write(&row.path, b"video data").unwrap();

    // Call DELETE missing-rows
    let (status, res) = fix
        .request(
            "DELETE",
            &format!("/api/v1/libraries/{}/missing-rows", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["data"]["deleted"], 0);
    assert_eq!(res["data"]["restored"], 1);

    // Ledger row still exists and unchanged
    let store_arc = fix.state.store();
    let guard = store_arc.lock();
    let loaded = guard.get_ledger(&row.id.to_string()).unwrap().unwrap();
    assert_eq!(loaded.id, row.id);
    assert_eq!(loaded.media_id, media_id);
}

#[tokio::test]
async fn parent_verify_and_prune_do_not_touch_nested_library() {
    let fix = PresenceFixture::new();
    let (_, parent_row) = fix.insert_media_and_ledger("parent.mkv");

    // Create nested library inside parent root
    let nested_root = fix.root.join("nested");
    std::fs::create_dir_all(&nested_root).unwrap();
    let _nested_lib = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard
            .create_library(
                MediaKind::Movie,
                "Nested",
                &[nested_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
    };

    // Add row to nested library
    let nested_file = nested_root.join("child.mkv");
    let (_nested_media_id, nested_row) = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        let mid = domain::MediaId::new();
        let m = domain::Media {
            id: mid,
            kind: MediaKind::Movie,
            title: "Child".into(),
            original_title: None,
            year: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        let r = domain::LedgerRow {
            id: domain::LedgerId::new(),
            media_id: mid,
            path: nested_file.to_str().unwrap().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Release,
            confidence: domain::Confidence::High,
            filter_score: None,
        };
        guard.insert_media(&m).unwrap();
        guard.insert_ledger(&r).unwrap();
        (mid, r)
    };

    // Both files are missing on disk
    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    // Parent verify only sees parent.mkv, ignores nested child.mkv
    assert_eq!(res["data"]["missing"], 1);

    // Prune on parent
    let (status, res) = fix
        .request(
            "DELETE",
            &format!("/api/v1/libraries/{}/missing-rows", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["data"]["deleted"], 1);

    // Parent row is deleted, but nested child row remains!
    let store_arc = fix.state.store();
    let guard = store_arc.lock();
    assert!(guard.get_ledger(&parent_row.id.to_string()).unwrap().is_none());
    assert!(guard.get_ledger(&nested_row.id.to_string()).unwrap().is_some());
}

#[tokio::test]
async fn root_failure_rejects_whole_request_without_writes() {
    let fix = PresenceFixture::new();
    let (_, _row) = fix.insert_media_and_ledger("one.mkv");

    // Make root unreadable
    fix.mock_files.unreadable_roots.lock().push(fix.root.clone());

    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(res["error"]["code"], "library.root_unavailable");

    let store_arc = fix.state.store();
    let guard = store_arc.lock();
    assert!(guard.missing_at_by_path().unwrap().is_empty());
}

#[tokio::test]
async fn ambiguous_scope_rejects_without_default_fallback() {
    let fix = PresenceFixture::new();
    // Create another library with the identical root
    {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard
            .create_library(
                MediaKind::Movie,
                "Movies Duplicate",
                &[fix.root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
    }
    let (_, _row) = fix.insert_media_and_ledger("one.mkv");

    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(res["error"]["code"], "library.ownership_ambiguous");
}

#[tokio::test]
async fn file_permission_error_is_skipped_not_missing() {
    let fix = PresenceFixture::new();
    let (_, row) = fix.insert_media_and_ledger("one.mkv");

    // Set permission error for file
    fix.mock_files.files.lock().insert(
        std::path::PathBuf::from(&row.path),
        Err(std::io::ErrorKind::PermissionDenied),
    );

    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["data"]["missing"], 0);
    assert_eq!(res["data"]["skipped"], 1);

    let store_arc = fix.state.store();
    let guard = store_arc.lock();
    assert!(guard.missing_at_by_path().unwrap().is_empty());
}

#[tokio::test]
async fn presence_routes_keep_admin_and_envelope_contract() {
    let fix = PresenceFixture::new();

    // Member gets 403 Forbidden
    let (status, _) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.member_token),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Non-existent library gets 404
    let (status, res) = fix
        .request(
            "POST",
            "/api/v1/libraries/non_existent_id/verify",
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(res["error"]["code"], "library.missing");

    // Invalid media_item_id query gets 400
    let (status, res) = fix
        .request(
            "DELETE",
            &format!(
                "/api/v1/libraries/{}/missing-rows?media_item_id=not-a-uuid",
                fix.library_id
            ),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(res["error"]["code"], "invalid_media_id");
}
