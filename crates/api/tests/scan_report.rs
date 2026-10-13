#[path = "support/mod.rs"]
mod support;

use axum::http::StatusCode;
use domain::MediaKind;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn multi_root_partial_scan_report() {
    let fix = PresenceFixture::new();

    // Create separate root1 and missing root2 so it doesn't overlap with fix.library_id root!
    let root1 = fix.dir.path().join("distinct_root1");
    let root2 = fix.dir.path().join("missing_root2");
    std::fs::create_dir_all(&root1).unwrap();

    let lib2 = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard
            .create_library(
                MediaKind::Movie,
                "Multi Root",
                &[root1.to_str().unwrap(), root2.to_str().unwrap()],
                "everyone",
                false,
                &[],
            )
            .unwrap()
    };

    let p_file = root1.join("valid_movie (2024).mkv");
    std::fs::write(&p_file, b"valid movie").unwrap();

    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/scan", lib2.id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["ok"], true);
    assert_eq!(res["data"]["status"], "partial");
    assert_eq!(res["data"]["queued"], true);
    assert_eq!(res["data"]["transferred"], true);
    assert_eq!(res["data"]["inserted"], 1);

    let roots = res["data"]["roots"].as_array().unwrap();
    assert_eq!(roots.len(), 2);
    assert!(roots.iter().any(|r| r["status"] == "completed"));
    assert!(roots.iter().any(|r| r["status"] == "unreachable"));
}

#[tokio::test]
async fn video_library_supports_mts_and_vob_and_iso() {
    let fix = PresenceFixture::new();
    let video_root = fix.dir.path().join("video_formats");
    std::fs::create_dir_all(&video_root).unwrap();

    let v_lib = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard
            .create_library(
                MediaKind::Video,
                "Formats Video",
                &[video_root.to_str().unwrap()],
                "everyone",
                false,
                &[],
            )
            .unwrap()
    };

    let f1 = video_root.join("sample.mts");
    let f2 = video_root.join("sample.vob");
    let f3 = video_root.join("sample.iso");
    let f_invalid = video_root.join("sample.txt");

    std::fs::write(&f1, b"mts").unwrap();
    std::fs::write(&f2, b"vob").unwrap();
    std::fs::write(&f3, b"iso").unwrap();
    std::fs::write(&f_invalid, b"txt").unwrap();

    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/scan", v_lib.id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["data"]["inserted"], 3);

    let store_arc = fix.state.store();
    let guard = store_arc.lock();
    assert!(guard.ledger_by_path(&f1.display().to_string()).unwrap().is_some());
    assert!(guard.ledger_by_path(&f2.display().to_string()).unwrap().is_some());
    assert!(guard.ledger_by_path(&f3.display().to_string()).unwrap().is_some());
    assert!(guard.ledger_by_path(&f_invalid.display().to_string()).unwrap().is_none());
}
