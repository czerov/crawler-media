use std::path::Path;

use domain::MediaKind;
use store::{Library, Store};

fn create(store: &Store, kind: MediaKind, roots: &[&Path]) -> Library {
    let roots: Vec<&str> = roots.iter().map(|root| root.to_str().unwrap()).collect();
    let name = format!("lib-{}", roots.first().map(|s| *s).unwrap_or("empty"));
    store
        .create_library(kind, &name, &roots, "everyone", true, &[])
        .unwrap()
}

#[test]
fn strict_scope_does_not_fall_back_to_default() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let _lib = create(&store, MediaKind::Movie, &[&root]);

    let outside = tmp.path().join("outside/one.mkv");
    let snapshot = store.library_ownership().unwrap();
    assert!(snapshot.owner(&outside, MediaKind::Movie).is_some());

    let scope = store::LibraryPathScope::resolve(snapshot).unwrap();
    let resolved = store::LibraryPathScope::resolve_path(&outside).unwrap();
    assert!(matches!(
        scope.owner(&resolved, MediaKind::Movie),
        store::StrictOwner::Unmatched
    ));
}

#[test]
fn nested_root_wins_and_sibling_prefix_does_not_match() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let parent_root = tmp.path().join("media");
    let nested_root = tmp.path().join("media/special");
    std::fs::create_dir_all(&nested_root).unwrap();

    let parent_lib = create(&store, MediaKind::Movie, &[&parent_root]);
    let nested_lib = create(&store, MediaKind::Movie, &[&nested_root]);

    let scope = store::LibraryPathScope::resolve(store.library_ownership().unwrap()).unwrap();

    let nested_file = nested_root.join("sample.mkv");
    let resolved_nested = store::LibraryPathScope::resolve_path(&nested_file).unwrap();
    match scope.owner(&resolved_nested, MediaKind::Movie) {
        store::StrictOwner::Owned(lib) => assert_eq!(lib.id, nested_lib.id),
        _ => panic!("expected nested library to own nested path"),
    }

    let parent_file = parent_root.join("sample.mkv");
    let resolved_parent = store::LibraryPathScope::resolve_path(&parent_file).unwrap();
    match scope.owner(&resolved_parent, MediaKind::Movie) {
        store::StrictOwner::Owned(lib) => assert_eq!(lib.id, parent_lib.id),
        _ => panic!("expected parent library to own parent path"),
    }

    let sibling_path = tmp.path().join("media-other/sample.mkv");
    let resolved_sibling = store::LibraryPathScope::resolve_path(&sibling_path).unwrap();
    assert!(matches!(
        scope.owner(&resolved_sibling, MediaKind::Movie),
        store::StrictOwner::Unmatched
    ));
}

#[test]
fn equal_depth_is_ambiguous() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let shared_root = tmp.path().join("shared");
    std::fs::create_dir_all(&shared_root).unwrap();

    let _lib1 = create(&store, MediaKind::Movie, &[&shared_root]);
    let _lib2 = create(&store, MediaKind::Movie, &[&shared_root]);

    let scope = store::LibraryPathScope::resolve(store.library_ownership().unwrap()).unwrap();
    let file = shared_root.join("one.mkv");
    let resolved = store::LibraryPathScope::resolve_path(&file).unwrap();
    assert!(matches!(
        scope.owner(&resolved, MediaKind::Movie),
        store::StrictOwner::Ambiguous
    ));
}

#[test]
fn resolved_scope_preserves_symlink_and_missing_descendant_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let real_root = tmp.path().join("real_media");
    std::fs::create_dir_all(&real_root).unwrap();
    let symlink_root = tmp.path().join("symlink_media");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real_root, &symlink_root).unwrap();

    let lib = create(&store, MediaKind::Movie, &[&symlink_root]);
    let scope = store::LibraryPathScope::resolve(store.library_ownership().unwrap()).unwrap();

    // Missing descendant under real root or symlink root resolves to canonical path
    let missing_file = symlink_root.join("sub/missing.mkv");
    let resolved = store::LibraryPathScope::resolve_path(&missing_file).unwrap();
    match scope.owner(&resolved, MediaKind::Movie) {
        store::StrictOwner::Owned(owned) => {
            assert_eq!(owned.id, lib.id);
            // Library in StrictOwner retains original config roots
            assert_eq!(owned.root_paths[0], symlink_root);
        }
        _ => panic!("expected owned via symlinked root"),
    }
}

#[test]
fn scope_is_stale_after_root_kind_or_freeze_change() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();

    let lib = create(&store, MediaKind::Movie, &[&root]);
    let snapshot1 = store.library_ownership().unwrap();
    let scope = store::LibraryPathScope::resolve(snapshot1.clone()).unwrap();
    assert!(scope.matches(&snapshot1));

    // Freeze library
    store.begin_library_removal(&lib.id).unwrap();
    let snapshot2 = store.library_ownership().unwrap();
    assert!(!scope.matches(&snapshot2));
}

#[test]
fn different_kind_overlap_keeps_existing_kind_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let shared_root = tmp.path().join("mixed");
    std::fs::create_dir_all(&shared_root).unwrap();

    let movie_lib = create(&store, MediaKind::Movie, &[&shared_root]);
    let tv_lib = create(&store, MediaKind::Tv, &[&shared_root]);

    let scope = store::LibraryPathScope::resolve(store.library_ownership().unwrap()).unwrap();
    let file = shared_root.join("item.mkv");
    let resolved = store::LibraryPathScope::resolve_path(&file).unwrap();

    match scope.owner(&resolved, MediaKind::Movie) {
        store::StrictOwner::Owned(lib) => assert_eq!(lib.id, movie_lib.id),
        _ => panic!("expected movie lib for movie kind"),
    }
    match scope.owner(&resolved, MediaKind::Tv) {
        store::StrictOwner::Owned(lib) => assert_eq!(lib.id, tv_lib.id),
        _ => panic!("expected tv lib for tv kind"),
    }
}
