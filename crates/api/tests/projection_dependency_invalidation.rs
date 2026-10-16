use std::sync::Arc;

use api::browse_projection::changes::{ChangeConsumer, ProjectionDependencies};
use domain::{Confidence, LedgerId, LedgerRow, MediaId, MediaKind, QualitySource};
use parking_lot::Mutex;
use store::{BrowseChangeSource, Store};

fn create_store() -> (tempfile::TempDir, Store) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    (tmp, store)
}

fn sample_ledger_row(id: LedgerId, media_id: MediaId, path: &str) -> LedgerRow {
    LedgerRow {
        id,
        media_id,
        path: path.to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    }
}

#[test]
fn unrelated_catalog_search_does_not_rebuild_other_library() {
    let (_tmp, store) = create_store();
    let deps = Arc::new(Mutex::new(ProjectionDependencies::new()));
    let consumer = ChangeConsumer::new(deps.clone());

    // Setup library 1
    let lib1_id = "lib-1";
    let media1_id = MediaId::new();
    let lid1 = LedgerId::new();
    deps.lock().record_library(
        lib1_id,
        [media1_id],
        [lid1],
        ["/movies/M1/m1.mkv".to_string()],
        ["tmdb:movie:100".to_string()],
    );

    // Initial dirty token for lib-1
    store
        .apply_browse_changes(BrowseChangeSource::App, 0, &[lib1_id.to_string()])
        .unwrap();
    let initial_head = store.browse_projection_head(lib1_id).unwrap().unwrap();

    // Insert unrelated catalog search
    store
        .put_catalog_cache("tmdb", "search:movie:avatar", "[]", 1000, None)
        .unwrap();

    let ownership = store.library_ownership().unwrap();
    let (_count, complete) = consumer
        .poll_source(&store, &ownership, BrowseChangeSource::Catalog)
        .unwrap();
    assert!(complete);

    // lib-1 dirty_token should remain unchanged!
    let unchanged_head = store.browse_projection_head(lib1_id).unwrap().unwrap();
    assert_eq!(unchanged_head.dirty_token, initial_head.dirty_token);
}

#[test]
fn catalog_detail_change_invalidates_unhydrated_library_coverage() {
    let (_tmp, store) = create_store();
    let ownership = store.library_ownership().unwrap();
    let library_id = ownership.libraries()[0].id.clone();
    let deps = Arc::new(Mutex::new(ProjectionDependencies::new()));
    let consumer = ChangeConsumer::new(deps);
    store
        .put_catalog_cache("tmdb", "/movie/999", "{}", 1, None)
        .unwrap();
    consumer
        .poll_source(&store, &ownership, BrowseChangeSource::Catalog)
        .unwrap();
    let after = store.browse_projection_head(&library_id).unwrap().unwrap();
    assert!(after.dirty_token > 0);
}
fn moving_ledger_dirties_old_and_new_owner() {
    let (tmp, store) = create_store();
    let deps = Arc::new(Mutex::new(ProjectionDependencies::new()));
    let consumer = ChangeConsumer::new(deps.clone());

    // Create two libraries with distinct roots
    let root_a = tmp.path().join("root_a");
    let root_b = tmp.path().join("root_b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();

    let lib_a = store
        .create_library(
            MediaKind::Movie,
            "Lib A",
            &[root_a.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let lib_b = store
        .create_library(
            MediaKind::Movie,
            "Lib B",
            &[root_b.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    // Initialize heads
    store
        .apply_browse_changes(
            BrowseChangeSource::Library,
            0,
            &[lib_a.id.clone(), lib_b.id.clone()],
        )
        .unwrap();
    let initial_old = store.browse_projection_head(&lib_a.id).unwrap().unwrap();
    let initial_new = store.browse_projection_head(&lib_b.id).unwrap().unwrap();

    let old_path = root_a.join("movie.mkv").display().to_string();
    let new_path = root_b.join("movie.mkv").display().to_string();

    let lid = LedgerId::new();
    let mid = MediaId::new();
    let row = sample_ledger_row(lid, mid, &old_path);
    store.insert_ledger(&row).unwrap();

    // Record lib-a dependency on old_path
    deps.lock().record_library(&lib_a.id, [mid], [lid], [old_path.clone()], []);

    // Clear cursor for clean move test
    let _ = consumer.poll_source(
        &store,
        &store.library_ownership().unwrap(),
        BrowseChangeSource::Library,
    );

    // Move row from A to B: delete old ledger path and insert new ledger row at new path with same media id
    store.delete_ledger_path(&old_path).unwrap();
    let new_lid = LedgerId::new();
    let new_row = sample_ledger_row(new_lid, mid, &new_path);
    store.insert_ledger(&new_row).unwrap();

    // Poll changes
    let ownership = store.library_ownership().unwrap();
    let (count, _) = consumer
        .poll_source(&store, &ownership, BrowseChangeSource::Library)
        .unwrap();
    assert!(count > 0);

    let old_owner_head = store.browse_projection_head(&lib_a.id).unwrap().unwrap();
    let new_owner_head = store.browse_projection_head(&lib_b.id).unwrap().unwrap();

    assert!(old_owner_head.dirty_token > initial_old.dirty_token);
    assert!(new_owner_head.dirty_token > initial_new.dirty_token);
}

#[test]
fn new_dirty_during_build_is_not_swallowed() {
    let (_tmp, store) = create_store();
    let lib_id = "lib-dirty";

    // Set initial dirty token = 1
    store
        .apply_browse_changes(BrowseChangeSource::Library, 0, &[lib_id.to_string()])
        .unwrap();
    let initial = store.browse_projection_head(lib_id).unwrap().unwrap();
    assert_eq!(initial.dirty_token, 1);

    // Prepare a build with expected_dirty_token = 1
    let build = store::ProjectionBuild {
        library_id: lib_id.to_string(),
        generation: 1,
        scope_signature: "sig1".to_string(),
        revisions: store.read_revisions().unwrap(),
        expected_dirty_token: Some(1),
        files: vec![],
        media: vec![],
    };

    // Before publish commits, a new change arrives and increments dirty_token to 2!
    store
        .apply_browse_changes(BrowseChangeSource::Library, 1, &[lib_id.to_string()])
        .unwrap();
    let updated_head = store.browse_projection_head(lib_id).unwrap().unwrap();
    assert_eq!(updated_head.dirty_token, 2);

    // Publish with stale expected_dirty_token (1) must fail (CAS check: return Ok(false))
    let published = store.publish_browse_projection(&build).unwrap();
    assert!(!published, "Publish must be rejected when dirty token advanced during build");
}
