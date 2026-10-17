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

    let root = _tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let library_id = library.id;

    let _ = consumer.poll_source(
        &store,
        &store.library_ownership().unwrap(),
        BrowseChangeSource::App,
    );
    let _ = consumer.poll_source(
        &store,
        &store.library_ownership().unwrap(),
        BrowseChangeSource::Library,
    );
    let initial_head = store.browse_projection_head(&library_id).unwrap().unwrap();

    // Insert unrelated catalog search
    store
        .put_catalog_cache("tmdb", "search:movie:avatar", "[]", 1000, None)
        .unwrap();

    let ownership = store.library_ownership().unwrap();
    let (_count, complete) = consumer
        .poll_source(&store, &ownership, BrowseChangeSource::Catalog)
        .unwrap();
    assert!(complete);

    let unchanged_head = store.browse_projection_head(&library_id).unwrap().unwrap();
    assert_eq!(unchanged_head.dirty_token, initial_head.dirty_token);
    assert_eq!(unchanged_head.generation, initial_head.generation);
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

#[test]
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

    // Model both libraries as having published projections before the ledger change.
    store
        .apply_browse_changes(
            BrowseChangeSource::Library,
            0,
            &[lib_a.id.clone(), lib_b.id.clone()],
        )
        .unwrap();

    // Initialize heads and drain fixture setup journal before capturing baselines.
    let _ = consumer.poll_source(
        &store,
        &store.library_ownership().unwrap(),
        BrowseChangeSource::Library,
    );
    let old_path = root_a.join("movie.mkv").display().to_string();
    let new_path = root_b.join("movie.mkv").display().to_string();

    let lid = LedgerId::new();
    let mid = MediaId::new();
    let row = sample_ledger_row(lid, mid, &old_path);
    store.insert_ledger(&row).unwrap();
    deps.lock().record_library(&lib_a.id, [mid], [lid], [old_path.clone()], []);
    let _ = consumer.poll_source(
        &store,
        &store.library_ownership().unwrap(),
        BrowseChangeSource::Library,
    );
    let initial_old = store.browse_projection_head(&lib_a.id).unwrap().unwrap();
    let initial_new = store.browse_projection_head(&lib_b.id).unwrap().unwrap();
    // Baselines follow setup journal drain; the move must advance both owner tokens.
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
        candidates: vec![],
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



#[test]
fn stale_library_dependencies_are_evicted_before_invalidation() {
    let (_tmp, store) = create_store();
    let ownership = store.library_ownership().unwrap();
    let removed_library_id = "removed-library";
    let deps = Arc::new(Mutex::new(ProjectionDependencies::new()));
    let consumer = ChangeConsumer::new(deps.clone());
    consumer.hydrate_ready_dependencies(&store).unwrap();
    deps.lock().record_library(
        removed_library_id,
        [MediaId::new()],
        [LedgerId::new()],
        ["/removed/library/file.mkv".to_string()],
        ["tmdb:/movie/42".to_string()],
    );
    consumer.hydrate_ready_dependencies(&store).unwrap();

    store
        // 台账键与记录的依赖键必须同形（journal key = `source:cache_key`），否则
        // 匹配永远失败，断言恒真、测不到“已删除库仍被扫描”。
        .put_catalog_cache("tmdb", "/movie/42", "{}", 1, None)
        .unwrap();
    let changes = store
        .browse_changes(BrowseChangeSource::Catalog, 0, 10)
        .unwrap();
    assert!(!consumer
        .resolve_affected(&store, &ownership, &changes)
        .iter()
        .any(|affected| affected == removed_library_id));
}
#[test]
fn acknowledged_coalescing_sentinel_is_pruned_after_interrupted_acknowledgement() {
    let (_tmp, store) = create_store();
    let ownership = store.library_ownership().unwrap();
    let library_id = ownership.libraries()[0].id.clone();
    store
        .apply_browse_changes(BrowseChangeSource::App, 0, &[library_id.clone()])
        .unwrap();
    let baseline = store.browse_projection_head(&library_id).unwrap().unwrap();
    for index in 0..=1024 {
        store
            .put_catalog_cache("tmdb", &format!("movie:{index}"), "[]", index, None)
            .unwrap();
    }

    let sentinel = store
        .browse_changes(BrowseChangeSource::Catalog, 0, 2048)
        .unwrap()
        .last()
        .unwrap()
        .clone();

    let catalog_path = store.data_dir().join("catalog.db");
    let connection = rusqlite::Connection::open(&catalog_path).unwrap();
    connection
        .execute_batch("CREATE TRIGGER fail_journal_prune BEFORE DELETE ON _browse_changes BEGIN SELECT RAISE(ABORT, 'simulated crash before pruning'); END;")
        .unwrap();
    assert!(store
        .apply_browse_changes(BrowseChangeSource::Catalog, sentinel.seq, &[])
        .is_err());
    connection
        .execute_batch("DROP TRIGGER fail_journal_prune;")
        .unwrap();

    let consumer = ChangeConsumer::new(Arc::new(Mutex::new(ProjectionDependencies::new())));
    store
        .put_catalog_cache("tmdb", "/movie/999", "{}", 10_000, None)
        .unwrap();
    assert!(store
        .browse_changes(BrowseChangeSource::Catalog, store.browse_change_cursor(BrowseChangeSource::Catalog).unwrap(), 10)
        .unwrap()
        .is_empty());
    let mut replay_budget = api::browse_projection::changes::MAX_EVENTS_PER_ROUND;
    let replay = consumer
        .poll_source_budgeted(
            &store,
            &ownership,
            BrowseChangeSource::Catalog,
            &mut replay_budget,
        )
        .unwrap();
    assert_eq!(replay, (1, true), "replay must consume exactly one budget unit");
    assert_eq!(
        replay_budget,
        api::browse_projection::changes::MAX_EVENTS_PER_ROUND - 1,
        "replay must decrement the caller-shared budget exactly once",
    );
    let after_replay = store.browse_projection_head(&library_id).unwrap().unwrap();
    assert!(
        after_replay.dirty_token > baseline.dirty_token,
        "replayed sentinel must invalidate the affected library before any later event",
    );
    assert!(store
        .browse_changes(BrowseChangeSource::Catalog, sentinel.seq - 1, 1)
        .unwrap()
        .is_empty(), "replay must prune the acknowledged coalescing sentinel");
    store
        .put_catalog_cache("tmdb", "/movie/1000", "{}", 10_001, None)
        .unwrap();
    let after_replay = consumer
        .poll_source(&store, &ownership, BrowseChangeSource::Catalog)
        .unwrap();
    assert_eq!(after_replay, (1, true), "a later catalog change must be consumable");
    let updated = store.browse_projection_head(&library_id).unwrap().unwrap();
    assert!(updated.dirty_token > baseline.dirty_token);

    let idle = consumer
        .poll_source(&store, &ownership, BrowseChangeSource::Catalog)
        .unwrap();
    assert_eq!(idle, (0, true), "an idle poll must process no events");
}
