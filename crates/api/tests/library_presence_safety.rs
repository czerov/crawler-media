mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use domain::MediaKind;
use support::library_presence::PresenceFixture;
use tokio::sync::Notify;

#[tokio::test]
async fn prune_preserves_other_library_and_user_facts() {
    let fix = PresenceFixture::new();
    let (media_id, row) = fix.insert_media_and_ledger("target.mkv");

    // Set user playback progress and favorite
    let user_id = domain::UserId::new();
    {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard
            .set_playback_state(user_id, media_id, 3000, Some(true), Some(true), None, None)
            .unwrap();
    }

    // Verify to mark it missing
    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["data"]["missing"], 1);

    // Prune the missing row
    let (status, res) = fix
        .request(
            "DELETE",
            &format!("/api/v1/libraries/{}/missing-rows", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["data"]["deleted"], 1);

    // Verify ledger row is deleted
    let store_arc = fix.state.store();
    let guard = store_arc.lock();
    assert!(guard.get_ledger(&row.id.to_string()).unwrap().is_none());

    // User playback facts and Media record are PRESERVED (S1)
    assert!(guard.get_media(media_id).unwrap().is_some());
    assert_eq!(
        guard.playback_progress(user_id, media_id).unwrap(),
        Some(3000)
    );
}

#[tokio::test]
async fn frozen_or_deleted_library_rejects_presence_operations() {
    let fix = PresenceFixture::new();
    let (_media_id, _row) = fix.insert_media_and_ledger("target.mkv");

    // Freeze library
    {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.begin_library_removal(&fix.library_id).unwrap();
    }

    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(res["error"]["code"], "library.deleting");

    let (status, res) = fix
        .request(
            "DELETE",
            &format!("/api/v1/libraries/{}/missing-rows", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(res["error"]["code"], "library.deleting");
}

/// 在 `readable_root`（scope 已捕获、发布屏障尚未进入）里执行一次生命周期变更。
fn install_root_hook<F>(fix: &PresenceFixture, action: F)
where
    F: Fn(&api::Store, &str) + Send + Sync + 'static,
{
    let store = fix.state.store();
    let library_id = fix.library_id.clone();
    let fired = AtomicBool::new(false);
    *fix.mock_files.on_readable_root.lock() = Some(Box::new(move |_path| {
        if fired.swap(true, Ordering::SeqCst) {
            return;
        }
        action(&store.lock(), &library_id);
    }));
}

/// 发布屏障内的 scope 换代必须按类型判成 `ScopeChanged`（409
/// `library.scope_changed`）。按文案分类的旧实现会把它落成伪造的 Store 故障
/// （500 `internal_error`）：同一个生命周期竞争被当成真实故障。
#[tokio::test]
async fn scope_change_race_is_classified_as_scope_changed() {
    let fix = PresenceFixture::new();
    let (_media_id, row) = fix.insert_media_and_ledger("target.mkv");
    // 第二个 Library 在 scope 捕获之后被冻结：`accepts_work` 集合换代，
    // 目标 Library 本身仍然接受工作（不会被误判成 Deleting）。
    let other_root = fix.dir.path().join("other");
    std::fs::create_dir_all(&other_root).unwrap();
    let other_id = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .create_library(
                MediaKind::Movie,
                "Other",
                &[other_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
    };
    install_root_hook(&fix, move |store, _library_id| {
        store.begin_library_removal(&other_id).unwrap();
    });

    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{res}");
    assert_eq!(res["error"]["code"], "library.scope_changed");

    // 竞争发生时屏障整体回滚：不写任何 presence 观察，也不动账本行。
    let guard = fix.state.store();
    let guard = guard.lock();
    assert!(guard.missing_at_by_path().unwrap().is_empty());
    assert!(guard.get_ledger(&row.id.to_string()).unwrap().is_some());
}

#[tokio::test]
async fn prune_with_permission_denied_on_recheck_skips_and_preserves_ledger() {
    let fix = PresenceFixture::new();
    let (_media_id, row) = fix.insert_media_and_ledger("perm_denied.mkv");
    let file_path = std::path::PathBuf::from(&row.path);

    // 1. First observation: Ok(false) -> marked missing
    fix.mock_files
        .files
        .lock()
        .insert(file_path.clone(), Ok(false));
    let (status, res) = fix
        .request(
            "POST",
            &format!("/api/v1/libraries/{}/verify", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["data"]["missing"], 1);

    // 2. Before prune: file has permission denied on disk
    fix.mock_files
        .files
        .lock()
        .insert(file_path, Err(std::io::ErrorKind::PermissionDenied));

    // 3. Prune missing-rows: must NOT delete unverified/permission-denied files
    let (status, res) = fix
        .request(
            "DELETE",
            &format!("/api/v1/libraries/{}/missing-rows", fix.library_id),
            Some(&fix.admin_token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["data"]["deleted"], 0);
    assert_eq!(res["data"]["skipped"], 1);

    // 4. Ledger row is safely preserved!
    let store_arc = fix.state.store();
    let guard = store_arc.lock();
    assert!(guard.get_ledger(&row.id.to_string()).unwrap().is_some());
}
