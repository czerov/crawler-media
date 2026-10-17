use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use store::{Store, StoredMediaMarker};

fn library(store: &Store, name: &str, root: &std::path::Path) {
    std::fs::create_dir_all(root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            name,
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
}

fn media(store: &Store, title: &str) -> Media {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: title.into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    media
}

fn ledger(store: &Store, media: &Media, path: &std::path::Path) -> LedgerRow {
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: path.display().to_string(),
        season: Some(1),
        episode: Some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).unwrap();
    row
}

fn marker(media: &Media, locked: bool) -> StoredMediaMarker {
    StoredMediaMarker {
        media_id: media.id,
        season: 1,
        episode: 1,
        intro_start_ms: Some(10_000),
        intro_end_ms: Some(70_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "detected".into(),
        locked,
        updated_at: 0,
    }
}

fn remove(store: &Store, library_id: &str) -> store::LibraryRemoval {
    let removal = store.begin_library_removal(library_id).unwrap();
    store
        .set_removal_state(&removal.operation_id, "cleaning", None)
        .unwrap();
    let done = store.clean_library_removal(&removal.operation_id).unwrap();
    assert_eq!(done.state, "succeeded");
    done
}

#[test]
fn removal_deletes_library_scoped_settings_and_keeps_other_libraries() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    library(&store, "A", &temp.path().join("tv_a"));
    library(&store, "B", &temp.path().join("tv_b"));
    let (a, b) = {
        let libs = store.list_libraries().unwrap();
        let a = libs.iter().find(|l| l.name == "A").unwrap().id.clone();
        let b = libs.iter().find(|l| l.name == "B").unwrap().id.clone();
        (a, b)
    };
    let key_a = format!("library_cover_checked.{a}");
    let key_b = format!("library_cover_checked.{b}");
    store.put_setting(&key_a, "checked-v2").unwrap();
    store.put_setting(&key_b, "checked-v2").unwrap();

    remove(&store, &a);

    assert!(store.get_setting(&key_a).unwrap().is_none());
    assert_eq!(
        store.get_setting(&key_b).unwrap().as_deref(),
        Some("checked-v2")
    );
}

#[test]
fn removal_keeps_locked_markers_and_clears_unlocked_ones() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let root = temp.path().join("tv_a");
    library(&store, "A", &root);
    let library_id = store
        .list_libraries()
        .unwrap()
        .iter()
        .find(|l| l.name == "A")
        .unwrap()
        .id
        .clone();
    let locked_media = media(&store, "Locked");
    let unlocked_media = media(&store, "Unlocked");
    ledger(&store, &locked_media, &root.join("locked.strm"));
    ledger(&store, &unlocked_media, &root.join("unlocked.strm"));
    store
        .put_media_marker(&marker(&locked_media, true))
        .unwrap();
    store
        .put_media_marker(&marker(&unlocked_media, false))
        .unwrap();

    remove(&store, &library_id);

    let kept = store
        .get_media_marker(locked_media.id, Some(1), Some(1))
        .unwrap()
        .expect("locked marker is user intent and must survive");
    assert!(kept.locked);
    assert_eq!(kept.intro_start_ms, Some(10_000));
    assert!(store
        .get_media_marker(unlocked_media.id, Some(1), Some(1))
        .unwrap()
        .is_none());
}

#[test]
fn removed_root_stays_a_boundary_and_can_be_reconfigured_as_a_new_library() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("data")).unwrap();
    let root_a = temp.path().join("tv_a");
    let root_b = temp.path().join("tv_b");
    library(&store, "A", &root_a);
    library(&store, "B", &root_b);
    let a = store
        .list_libraries()
        .unwrap()
        .iter()
        .find(|l| l.name == "A")
        .unwrap()
        .id
        .clone();

    remove(&store, &a);

    assert!(store
        .path_in_removal_scope(&root_a.join("E01.strm"))
        .unwrap());
    assert!(!store
        .path_in_removal_scope(&root_b.join("E01.strm"))
        .unwrap());

    // Re-adding the removed directory is still allowed and owns its paths again.
    library(&store, "A2", &root_a);
    let owner = store
        .library_for_path_strict(&root_a.join("E01.strm"), MediaKind::Tv)
        .unwrap()
        .expect("new Library owns the re-added root");
    assert_eq!(owner.name, "A2");
}

#[test]
fn removal_boundary_survives_reopen_without_the_per_file_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let root = temp.path().join("tv_a");
    let store = Store::open(&data).unwrap();
    library(&store, "A", &root);
    let library_id = store
        .list_libraries()
        .unwrap()
        .iter()
        .find(|l| l.name == "A")
        .unwrap()
        .id
        .clone();
    let media = media(&store, "Example");
    ledger(&store, &media, &root.join("E01.strm"));
    let done = remove(&store, &library_id);
    drop(store);

    let reopened = Store::open(&data).unwrap();
    assert!(reopened
        .path_in_removal_scope(&root.join("E01.strm"))
        .unwrap());
    assert_eq!(reopened.removal_roots(&done.operation_id).unwrap().len(), 1);
    assert!(reopened.get_library(&library_id).unwrap().is_none());
}

/// 真实升级边界：库已经是当前 app schema（7），`previous < APP_SCHEMA_VERSION`
/// 不成立。b3192f1 之前删除的 Library 留下的 `library_cover_checked.{id}` 仍然
/// 必须在重开时被清掉，而不是因为「版本已经是最新」被跳过。
#[test]
fn reopen_at_current_app_schema_cleans_settings_of_already_removed_library() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let store = Store::open(&data).unwrap();
    library(&store, "A", &temp.path().join("tv_a"));
    library(&store, "B", &temp.path().join("tv_b"));
    let (a, b) = {
        let libs = store.list_libraries().unwrap();
        let a = libs.iter().find(|l| l.name == "A").unwrap().id.clone();
        let b = libs.iter().find(|l| l.name == "B").unwrap().id.clone();
        (a, b)
    };
    let key_a = format!("library_cover_checked.{a}");
    let key_b = format!("library_cover_checked.{b}");
    remove(&store, &a);
    // Pre-fix residue: the removal succeeded, but a build that never ran the
    // repair wrote the Library's setting back (or never deleted it).
    store.put_setting(&key_a, "checked-v2").unwrap();
    store.put_setting(&key_b, "checked-v2").unwrap();
    let app_version = store
        .schema_versions()
        .into_iter()
        .find(|(name, _)| *name == "app")
        .unwrap()
        .1;
    assert_eq!(app_version, 8, "本测试钉住真实边界：app schema 8");
    drop(store);

    let reopened = Store::open(&data).unwrap();
    assert!(reopened.get_setting(&key_a).unwrap().is_none());
    assert_eq!(
        reopened.get_setting(&key_b).unwrap().as_deref(),
        Some("checked-v2")
    );
}
