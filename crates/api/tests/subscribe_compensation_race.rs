use domain::MediaId;
use store::Store;

#[test]
fn undo_ensured_media_guards_and_leaves_referenced_media() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let media_id = MediaId::new();
    let media = domain::Media {
        id: media_id,
        kind: domain::MediaKind::Movie,
        title: "Test Movie".into(),
        year: Some(2024),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();

    // With no reference, undo succeeds
    assert!(store.get_media(media_id).unwrap().is_some());
    let deleted = store.undo_ensured_media_if_unreferenced(media_id, &media).unwrap();
    assert!(deleted);
    assert!(store.get_media(media_id).unwrap().is_none());
}

fn movie(id: MediaId, title: &str) -> domain::Media {
    domain::Media {
        id,
        kind: domain::MediaKind::Movie,
        title: title.into(),
        year: Some(2024),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

/// 恢复旧 Media 前必须比较完整标识：并发请求改了别名/外部 ID 时，
/// 失败请求不得把后一个成功请求的写入覆盖回旧值。
#[test]
fn undo_ensured_media_refuses_when_identity_changed_concurrently() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    // 本次请求写入的新 Media（无别名冲突 → previous 为 None）。
    let (media, previous) = store
        .ensure_media_with_previous(movie(MediaId::new(), "Incoming"))
        .unwrap();
    assert!(previous.is_none());

    // 模拟并发请求在同一行上写入新身份（别名/外部 ID 变化）。
    let mut concurrent = media.clone();
    concurrent.tmdb_id = Some("603".into());
    concurrent.title = "Renamed By Concurrent Request".into();
    store.update_media(&concurrent).unwrap();

    let restored = store.undo_ensured_media(&media, previous.as_ref()).unwrap();
    assert!(!restored, "身份已被并发修改时必须拒绝恢复");
    let current = store.get_media(media.id).unwrap().unwrap();
    assert_eq!(current.title, "Renamed By Concurrent Request");
    assert_eq!(current.tmdb_id.as_deref(), Some("603"));
}

/// 别名合并路径：并发请求改了同一行时，恢复旧 Media 必须被 CAS 拒绝。
#[test]
fn undo_ensured_media_refuses_to_restore_over_concurrent_alias_change() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let existing_id = MediaId::new();
    let mut existing = movie(existing_id, "Existing");
    existing.tmdb_id = Some("1".into());
    store.insert_media(&existing).unwrap();

    let mut incoming = movie(MediaId::new(), "Incoming");
    incoming.tmdb_id = Some("1".into());
    let (media, previous) = store.ensure_media_with_previous(incoming).unwrap();
    assert_eq!(media.id, existing_id, "别名命中必须复用已有 Media");
    assert!(previous.is_some());

    let mut concurrent = media.clone();
    concurrent.original_title = Some("Concurrent Rewrite".into());
    store.update_media(&concurrent).unwrap();

    let restored = store.undo_ensured_media(&media, previous.as_ref()).unwrap();
    assert!(!restored, "并发改写后不得恢复旧身份");
    let current = store.get_media(existing_id).unwrap().unwrap();
    assert_eq!(current.original_title.as_deref(), Some("Concurrent Rewrite"));

    // 没有并发改写时，恢复必须成功且回到 previous 的内容。
    let restored = store
        .undo_ensured_media(&concurrent, previous.as_ref())
        .unwrap();
    assert!(restored, "身份未变时必须允许恢复");
    let reverted = store.get_media(existing_id).unwrap().unwrap();
    assert_eq!(reverted.title, previous.as_ref().unwrap().title);
}

/// 新建路径的补偿同样必须做完整 CAS：内容被改过就不得删除。
#[test]
fn undo_ensured_media_if_unreferenced_refuses_when_identity_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let media_id = MediaId::new();
    let created = movie(media_id, "Created");
    store.insert_media(&created).unwrap();

    let mut mutated = created.clone();
    mutated.tmdb_id = Some("42".into());
    store.update_media(&mutated).unwrap();

    let deleted = store
        .undo_ensured_media_if_unreferenced(media_id, &created)
        .unwrap();
    assert!(!deleted, "内容已被修改时不得删除");
    assert!(store.get_media(media_id).unwrap().is_some());
}
