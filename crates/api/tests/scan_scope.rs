#[path = "support/mod.rs"]
mod support;

use std::path::PathBuf;
use domain::MediaKind;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn full_movie_scan_skips_nested_independent_library() {
    let fix = PresenceFixture::new();

    // Create nested library inside parent root
    let nested_root = fix.root.join("nested");
    std::fs::create_dir_all(&nested_root).unwrap();
    let nested_lib = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard
            .create_library(
                MediaKind::Movie,
                "Nested",
                &[nested_root.to_str().unwrap()],
                "everyone",
                false, // realtime_watch = false
                &[],
            )
            .unwrap()
    };

    // Put a movie file inside parent root and another inside nested root
    let parent_file = fix.root.join("parent_movie (2024).mkv");
    let nested_file = nested_root.join("nested_movie (2024).mkv");
    std::fs::write(&parent_file, b"parent video").unwrap();
    std::fs::write(&nested_file, b"nested video").unwrap();

    // Trigger full scan on parent library
    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/scan", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(res["ok"], true);

    // Parent ledger contains parent_file, but does NOT contain nested_file!
    let store_arc = fix.state.store();
    let guard = store_arc.lock();
    let rows = guard.list_ledger().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, parent_file.display().to_string());

    // Nested file was NOT registered in ledger
    assert!(guard.ledger_by_path(&nested_file.display().to_string()).unwrap().is_none());
}

#[tokio::test]
async fn full_video_scan_skips_nested_independent_library() {
    let fix = PresenceFixture::new();

    // Create a video library
    let video_root = fix.dir.path().join("video_lib");
    let nested_video_root = video_root.join("sub_video");
    std::fs::create_dir_all(&nested_video_root).unwrap();

    let (video_lib, nested_video_lib) = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        let v1 = guard
            .create_library(
                MediaKind::Video,
                "Video Lib",
                &[video_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let v2 = guard
            .create_library(
                MediaKind::Video,
                "Nested Video Lib",
                &[nested_video_root.to_str().unwrap()],
                "everyone",
                false,
                &[],
            )
            .unwrap();
        (v1, v2)
    };

    let p_file = video_root.join("clip1.mp4");
    let n_file = nested_video_root.join("clip2.mp4");
    std::fs::write(&p_file, b"clip1").unwrap();
    std::fs::write(&n_file, b"clip2").unwrap();

    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/scan", video_lib.id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(res["ok"], true);

    let store_arc = fix.state.store();
    let guard = store_arc.lock();
    assert!(guard.ledger_by_path(&p_file.display().to_string()).unwrap().is_some());
    // Nested file was NOT registered under parent video library!
    assert!(guard.ledger_by_path(&n_file.display().to_string()).unwrap().is_none());
}

#[tokio::test]
async fn rescanning_restored_movie_clears_missing_without_overwriting_probe_quality() {
    let fix = PresenceFixture::new();
    let (media_id, row) = fix.insert_media_and_ledger("restored_movie (2024).mkv");

    // Mark missing
    let store_arc = fix.state.store();
    {
        let guard = store_arc.lock();
        guard
            .apply_ledger_presence(
                &[store::LedgerPresenceObservation {
                    row: row.clone(),
                    missing_at: None,
                    presence: store::FilePresence::Missing,
                }],
                store::LedgerPresenceMode::Verify,
                1000,
            )
            .unwrap();
        assert!(guard.missing_at_by_path().unwrap().contains_key(&row.path));
    }

    // Restore file on disk
    std::fs::write(&row.path, b"restored bytes").unwrap();

    // Scan library
    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/scan", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::OK);

    // missing_at must be cleared, ledger row and media id preserved
    let guard = store_arc.lock();
    assert!(!guard.missing_at_by_path().unwrap().contains_key(&row.path));
    let loaded = guard.get_ledger(&row.id.to_string()).unwrap().unwrap();
    assert_eq!(loaded.id, row.id);
    assert_eq!(loaded.media_id, media_id);
}
