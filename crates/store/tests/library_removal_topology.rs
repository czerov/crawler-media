use domain::MediaKind;
use store::{Store, StoreError};

fn protected<T: std::fmt::Debug>(result: Result<T, StoreError>) {
    assert!(
        matches!(result, Err(StoreError::Protected(_))),
        "{result:?}"
    );
}

#[test]
fn pending_snapshot_blocks_overlapping_create_and_repoint_without_partial_changes() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let root = temp.path().join("frozen");
    let a = store
        .create_library(
            MediaKind::Tv,
            "A",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let b = store
        .create_library(MediaKind::Tv, "B", &[], "everyone", true, &[])
        .unwrap();
    let removal = store.begin_library_removal(&a.id).unwrap();
    for candidate in [root.clone(), root.join("nested"), temp.path().to_path_buf()] {
        let count = store.list_libraries().unwrap().len();
        protected(store.create_library(
            MediaKind::Movie,
            "Blocked",
            &[candidate.to_str().unwrap()],
            "everyone",
            true,
            &[],
        ));
        assert_eq!(store.list_libraries().unwrap().len(), count);
        protected(store.update_library(
            &b.id,
            Some("Changed"),
            Some(&[candidate.to_str().unwrap()]),
            None,
        ));
        assert_eq!(store.get_library(&b.id).unwrap().unwrap().name, "B");
    }
    store
        .set_removal_state(&removal.operation_id, "failed", Some("retry"))
        .unwrap();
    protected(store.create_library(
        MediaKind::Tv,
        "Blocked",
        &[root.to_str().unwrap()],
        "everyone",
        true,
        &[],
    ));
    store
        .set_removal_state(&removal.operation_id, "cleaning", None)
        .unwrap();
    store.clean_library_removal(&removal.operation_id).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Reimport",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
}

#[test]
fn frozen_library_rejects_all_configuration_mutations_and_default_demotion() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let a = store.default_library(MediaKind::Tv).unwrap().unwrap();
    let b = store
        .create_library(MediaKind::Tv, "B", &[], "everyone", true, &[])
        .unwrap();
    store.begin_library_removal(&a.id).unwrap();
    protected(store.update_library(&a.id, Some("Changed"), None, None));
    protected(store.set_library_intro_settings(&a.id, true, true));
    protected(store.set_library_cover_path(&a.id, Some("cover.jpg")));
    protected(store.set_library_switch_settings(&a.id, Some(false), None, None, None, None));
    protected(store.set_default_library(&a.id));
    protected(store.set_default_library(&b.id));
    protected(store.reorder_libraries(&[&b.id, &a.id]));
    assert_eq!(store.get_library(&a.id).unwrap().unwrap(), a);
    assert_eq!(store.get_library(&b.id).unwrap().unwrap(), b);
}

#[cfg(unix)]
#[test]
fn pending_snapshot_blocks_symlink_and_lexical_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let root = temp.path().join("frozen");
    std::fs::create_dir_all(&root).unwrap();
    let alias = temp.path().join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    let a = store
        .create_library(
            MediaKind::Tv,
            "A",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store.begin_library_removal(&a.id).unwrap();
    for candidate in [alias.join("nested"), root.join("other/../nested")] {
        protected(store.create_library(
            MediaKind::Tv,
            "Blocked",
            &[candidate.to_str().unwrap()],
            "everyone",
            true,
            &[],
        ));
    }
}

#[test]
fn legacy_root_configuration_respects_frozen_owner_and_snapshot_overlap() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let a = store.default_library(MediaKind::Tv).unwrap().unwrap();
    let root = store
        .insert_library_root(MediaKind::Tv, temp.path().join("extra").to_str().unwrap())
        .unwrap();
    store
        .create_library(MediaKind::Tv, "B", &[], "everyone", true, &[])
        .unwrap();
    store.begin_library_removal(&a.id).unwrap();
    protected(store.set_library_root(MediaKind::Tv, "/changed"));
    protected(store.insert_library_root(MediaKind::Tv, "/new-root"));
    protected(store.delete_extra_library_root(&root.id));
    for path in &a.root_paths {
        protected(store.set_library_root(MediaKind::Movie, path.to_str().unwrap()));
        protected(store.insert_library_root(MediaKind::Movie, path.to_str().unwrap()));
    }
    assert_eq!(
        store.get_library(&a.id).unwrap().unwrap(),
        a_with_extra(a, root.path)
    );
}

fn a_with_extra(mut library: store::Library, extra: std::path::PathBuf) -> store::Library {
    library.root_paths.push(extra);
    library
}

#[test]
fn last_kind_counts_only_active_libraries_and_default_promotion_skips_frozen() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let a = store.default_library(MediaKind::Tv).unwrap().unwrap();
    let b = store
        .create_library(MediaKind::Tv, "B", &[], "everyone", true, &[])
        .unwrap();
    let c = store
        .create_library(MediaKind::Tv, "C", &[], "everyone", true, &[])
        .unwrap();
    let a_removal = store.begin_library_removal(&a.id).unwrap();
    let b_removal = store.begin_library_removal(&b.id).unwrap();
    protected(store.begin_library_removal(&c.id));
    store
        .set_removal_state(&b_removal.operation_id, "failed", Some("retry"))
        .unwrap();
    protected(store.begin_library_removal(&c.id));
    store
        .set_removal_state(&a_removal.operation_id, "cleaning", None)
        .unwrap();
    store
        .clean_library_removal(&a_removal.operation_id)
        .unwrap();
    assert_eq!(
        store.default_library(MediaKind::Tv).unwrap().unwrap().id,
        c.id
    );
    assert!(!store.get_library(&b.id).unwrap().unwrap().is_default);
}
