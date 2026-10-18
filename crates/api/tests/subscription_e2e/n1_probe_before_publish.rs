//! Review N-1: a wash-cut arrival that needs S1 approval is probed at the
//! source before anything is published; an unprobeable file never enters the
//! Library, and after a capped number of rounds the pending row is rejected.

use super::harness::*;

fn all_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            all_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[tokio::test]
async fn unprobeable_arrival_is_never_published_and_rejected_after_cap() {
    let h = Harness::new();
    h.create_site().await;
    let id = h
        .create_movie_subscribe(serde_json::json!({ "wash_cut": true }))
        .await;
    let owned = h.seed_owned_movie(id, 1080, "h264", 1);
    h.state
        .store()
        .lock()
        .put_setting("subscribe.probe_wait_max_attempts", "3")
        .unwrap();
    let before = {
        let mut files = Vec::new();
        all_files(&h.movie_library_root(), &mut files);
        files
    };
    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1);
    h.qb.complete(
        0,
        MATRIX_TITLE,
        &[(&format!("{MATRIX_TITLE}.mkv"), &unprobeable_video())],
    );

    let dir = owned.parent().unwrap().to_path_buf();
    let dir_mtime = || std::fs::metadata(&dir).unwrap().modified().unwrap();
    let mtime_before = dir_mtime();
    for round in 1..=3 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        h.transfer(id).await.unwrap();
        assert_eq!(
            dir_mtime(),
            mtime_before,
            "round {round}: library dir untouched (no publish)"
        );
        let mut files = Vec::new();
        all_files(&h.movie_library_root(), &mut files);
        assert_eq!(
            files, before,
            "round {round}: nothing published or left behind"
        );
        assert_eq!(
            h.ledger_paths(),
            vec![owned.display().to_string()],
            "round {round}"
        );
        assert!(
            h.state
                .store()
                .lock()
                .list_pending_file_deletions()
                .unwrap()
                .is_empty(),
            "round {round}: no publish/retract churn"
        );
        if round < 3 {
            assert_eq!(
                h.pending_state(id, "active"),
                1,
                "round {round}: still waiting"
            );
        }
    }
    assert_eq!(h.pending_state(id, "active"), 0, "probe wait capped");
    assert_eq!(
        h.pending_state(id, "rejected"),
        1,
        "capped wait ends as rejected"
    );
    assert!(owned.is_file());
}
