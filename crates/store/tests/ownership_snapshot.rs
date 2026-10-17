//! Request-local Library ownership snapshot: longest-root prefix rules,
//! equal-depth ambiguity without default fallback, and parity with
//! `Store::library_for_path` / `Store::library_accepts_work`.

use std::path::{Path, PathBuf};

use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use store::{Library, Store};

fn create(store: &Store, name: &str, roots: &[&Path]) -> Library {
    let roots: Vec<&str> = roots.iter().map(|root| root.to_str().unwrap()).collect();
    store
        .create_library(MediaKind::Movie, name, &roots, "everyone", true, &[])
        .unwrap()
}

fn ledger_row(media_id: MediaId, path: &Path) -> LedgerRow {
    LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: path.display().to_string(),
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

fn media() -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Same".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

#[test]
fn nested_root_wins_over_parent_root() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let parent_root = tmp.path().join("owned");
    let child_root = parent_root.join("child");
    let parent = create(&store, "parent", &[&parent_root]);
    let child = create(&store, "child", &[&child_root]);

    let view = store.library_ownership().unwrap();
    let parent_path = parent_root.join("one.mkv");
    let child_path = child_root.join("two.mkv");

    assert_eq!(view.owner(&parent_path, MediaKind::Movie).unwrap().id, parent.id);
    assert_eq!(view.owner(&child_path, MediaKind::Movie).unwrap().id, child.id);
    assert_eq!(
        store.library_for_path(&child_path, MediaKind::Movie).unwrap().unwrap().id,
        child.id
    );
}

#[test]
fn equal_depth_across_libraries_is_ambiguous_and_does_not_fall_back() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let shared = tmp.path().join("dup");
    let a = create(&store, "a", &[&shared]);
    let b = create(&store, "b", &[&shared]);
    assert_ne!(a.id, b.id);
    assert!(store.default_library(MediaKind::Movie).unwrap().is_some());

    let view = store.library_ownership().unwrap();
    let path = shared.join("x.mkv");
    assert!(view.owner(&path, MediaKind::Movie).is_none());
    assert!(store.library_for_path(&path, MediaKind::Movie).unwrap().is_none());
}

#[test]
fn unmatched_path_falls_back_to_kind_default() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let default = store.default_library(MediaKind::Movie).unwrap().unwrap();
    let outside = tmp.path().join("elsewhere/one.mkv");

    let view = store.library_ownership().unwrap();
    assert_eq!(view.default_for(MediaKind::Movie).unwrap().id, default.id);
    assert_eq!(view.owner(&outside, MediaKind::Movie).unwrap().id, default.id);
    assert_eq!(
        store.library_for_path(&outside, MediaKind::Movie).unwrap().unwrap().id,
        default.id
    );
}

#[test]
fn sibling_prefix_path_does_not_match_root() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let owned_root = tmp.path().join("owned");
    let lib = create(&store, "owned", &[&owned_root]);

    let view = store.library_ownership().unwrap();
    let sibling = tmp.path().join("owned-other/one.mkv");
    let owner = view.owner(&sibling, MediaKind::Movie).unwrap();
    assert_ne!(owner.id, lib.id);
    assert_eq!(owner.id, view.default_for(MediaKind::Movie).unwrap().id);
}

#[test]
fn same_media_in_two_libraries_owns_each_path_separately() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let parent_root = tmp.path().join("parent");
    let child_root = parent_root.join("child");
    let parent = create(&store, "parent", &[&parent_root]);
    let child = create(&store, "child", &[&child_root]);
    let media = media();
    store.insert_media(&media).unwrap();
    let parent_path = parent_root.join("one.mkv");
    let child_path = child_root.join("two.mkv");
    store.insert_ledger(&ledger_row(media.id, &parent_path)).unwrap();
    store.insert_ledger(&ledger_row(media.id, &child_path)).unwrap();

    let view = store.library_ownership().unwrap();
    assert_eq!(view.owner(&parent_path, MediaKind::Movie).unwrap().id, parent.id);
    assert_eq!(view.owner(&child_path, MediaKind::Movie).unwrap().id, child.id);
    assert_eq!(
        store.library_for_path(&parent_path, MediaKind::Movie).unwrap().unwrap().id,
        parent.id
    );
    assert_eq!(
        store.library_for_path(&child_path, MediaKind::Movie).unwrap().unwrap().id,
        child.id
    );
}

#[test]
fn snapshot_accepts_work_matches_store_helper_per_library() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    create(&store, "extra", &[&tmp.path().join("extra")]);

    let view = store.library_ownership().unwrap();
    assert!(!view.libraries().is_empty());
    for library in view.libraries() {
        assert_eq!(
            view.accepts_work(&library.id),
            store.library_accepts_work(&library.id).unwrap(),
            "library {}",
            library.id
        );
    }
    assert!(!view.accepts_work("missing-library"));
    assert!(!store.library_accepts_work("missing-library").unwrap());
}

#[test]
fn blocked_library_rejects_work_even_when_removal_succeeded() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let lib = create(&store, "frozen", &[&tmp.path().join("frozen")]);
    let conn = rusqlite::Connection::open(tmp.path().join("data/app.db")).unwrap();
    conn.execute(
        "INSERT INTO library_removals
            (operation_id, library_id, state, snapshot_json, removed_ledger_count,
             created_at_ms, updated_at_ms)
         VALUES ('op-succeeded', ?1, 'succeeded', '{\"roots\":[],\"rows\":[]}', 0, 0, 0)",
        rusqlite::params![lib.id],
    )
    .unwrap();
    drop(conn);

    let view = store.library_ownership().unwrap();
    assert!(view.library(&lib.id).is_some());
    assert!(!view.accepts_work(&lib.id));
    assert!(!store.library_accepts_work(&lib.id).unwrap());
}

#[test]
fn snapshot_preserves_root_order_not_lexical_order() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let z = tmp.path().join("zroot");
    let a = tmp.path().join("aroot");
    let m = tmp.path().join("mroot");
    let lib = create(&store, "ordered", &[&z, &a, &m]);
    let expected: Vec<PathBuf> = vec![z, a, m];
    assert_eq!(lib.root_paths, expected);

    let view = store.library_ownership().unwrap();
    assert_eq!(view.library(&lib.id).unwrap().root_paths, expected);
    assert_eq!(
        store.get_library(&lib.id).unwrap().unwrap().root_paths,
        expected
    );
}
