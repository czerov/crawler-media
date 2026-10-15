//! D-H3 + M8: importing into a Library whose canonical and first alternate
//! names are already occupied (user files, not in the ledger) must not
//! overwrite either; the import lands on the next free name.

use super::harness::*;

/// Import once in a scratch harness to learn the canonical relative path.
async fn canonical_relative_path() -> std::path::PathBuf {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    h.run_subscribe(id).await;
    h.qb.complete(0, MATRIX_TITLE, &[(&format!("{MATRIX_TITLE}.mkv"), &probed_video(2160, "hevc", false))]);
    h.transfer(id).await.unwrap();
    let files = h.library_files();
    assert_eq!(files.len(), 1);
    files[0].strip_prefix(h.movie_library_root()).unwrap().to_path_buf()
}

#[tokio::test]
async fn import_never_overwrites_occupied_names_and_picks_next_free_one() {
    let rel = canonical_relative_path().await;
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;

    let canonical = h.movie_library_root().join(&rel);
    let stem = canonical.file_stem().unwrap().to_str().unwrap().to_string();
    let ext = canonical.extension().unwrap().to_str().unwrap().to_string();
    let first_alt = canonical.with_file_name(format!("{stem}-{MATRIX_TITLE}.{ext}"));
    let second_alt = canonical.with_file_name(format!("{stem}-{MATRIX_TITLE}-2.{ext}"));
    std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
    std::fs::write(&canonical, b"user canonical").unwrap();
    std::fs::write(&first_alt, b"user alternate").unwrap();

    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1);
    let video = probed_video(2160, "hevc", false);
    let downloaded = h.qb.complete(0, MATRIX_TITLE, &[(&format!("{MATRIX_TITLE}.mkv"), &video)]);
    h.transfer(id).await.unwrap();

    assert_eq!(std::fs::read(&canonical).unwrap(), b"user canonical");
    assert_eq!(std::fs::read(&first_alt).unwrap(), b"user alternate");
    assert_eq!(std::fs::read(&second_alt).unwrap(), video, "import lands on the next free name");
    assert_eq!(h.ledger_paths(), vec![second_alt.display().to_string()]);
    assert!(downloaded[0].is_file(), "seeding source kept");
    assert_eq!(h.pending_state(id, "imported"), 1);

    // Retrying the Transfer is idempotent: no further copies appear.
    h.transfer(id).await.unwrap();
    assert_eq!(h.library_files().len(), 3);
}
