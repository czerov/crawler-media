//! Public regressions for persisted download ownership and destination-scoped Transfer.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use domain::{DownloaderId, SubscribeId, Torrent};
use downloader::{Downloader, DownloaderError, TaskSnapshot};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;
#[path = "delivery/fixture.rs"]
mod fixture;

#[derive(Default)]
struct OwnedDownloads {
    files: Mutex<HashMap<String, PathBuf>>,
    removed: Mutex<Vec<String>>,
    scans: Mutex<usize>,
}

impl OwnedDownloads {
    fn record_removal(&self, enclosure: &str, delete_files: bool) -> Result<(), DownloaderError> {
        self.removed.lock().push(enclosure.to_string());
        if delete_files {
            if let Some(path) = self.files.lock().get(enclosure) {
                std::fs::remove_file(path)?;
            }
        }
        Ok(())
    }
}

impl Downloader for OwnedDownloads {
    fn add(&self, _: &Torrent) -> Result<(), DownloaderError> {
        Ok(())
    }

    fn completed_files(&self, torrent: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        Ok(self
            .files
            .lock()
            .get(&torrent.enclosure)
            .cloned()
            .into_iter()
            .collect())
    }

    fn remove(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        self.remove_owned(torrent, delete_files)
    }

    fn owned_identity(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        Ok(downloader::magnet_info_hash(&torrent.enclosure))
    }

    fn remove_owned(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        if self.owned_identity(torrent)?.is_none() {
            return Err(DownloaderError::Message(
                "owned identity unproven: HTTP enclosure could not prove an actual downloader identity; refusing unsafe cleanup"
                    .into(),
            ));
        }
        self.record_removal(&torrent.enclosure, delete_files)
    }

    fn delete_task(&self, info_hash: &str, delete_files: bool) -> Result<(), DownloaderError> {
        let enclosure = self
            .files
            .lock()
            .keys()
            .find(|enclosure| downloader::magnet_info_hash(enclosure).as_deref() == Some(info_hash))
            .cloned()
            .unwrap_or_else(|| info_hash.to_string());
        self.record_removal(&enclosure, delete_files)
    }

    fn task_snapshots(&self) -> Result<Vec<TaskSnapshot>, DownloaderError> {
        *self.scans.lock() += 1;
        Ok(vec![TaskSnapshot {
            tag: downloader::TASK_TAG.into(),
            name: "The.Matrix.1999.1080p".into(),
            progress: 1.0,
            state: "completed".into(),
            size_bytes: 100,
            downloaded_bytes: 100,
            uploaded_bytes: 0,
            download_speed: 0,
            upload_speed: 0,
            info_hash: "unrelated".into(),
            owned_tags: vec![],
        }])
    }
}

fn setup(tmp: &tempfile::TempDir) -> (axum::Router, Store, Arc<OwnedDownloads>) {
    let downloads = Arc::new(OwnedDownloads::default());
    let state = api::ApiState::new(
        Store::open(tmp.path().join("data")).unwrap(),
        "management-secret".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloads.clone(),
        tmp.path().join("library"),
    )
    .unwrap();
    (
        router(state),
        Store::open(tmp.path().join("data")).unwrap(),
        downloads,
    )
}

fn pending(enclosure: &str, downloader_id: Option<DownloaderId>) -> api::store::PendingDownload {
    api::store::PendingDownload {
        torrent: Torrent {
            site_id: domain::SiteId::new(),
            title: "The.Matrix.1999.1080p".into(),
            enclosure: enclosure.into(),
            size_bytes: Some(100),
            seeders: None,
            free: false,
            hr: false,
            imdb_id: None,
            id: None,
            leechers: None,
            snatched: None,
            upload_time: None,
            detail_url: None,
            category: None,
            poster_url: None,
        },
        release_override: None,
        downloader_id,
        submitted_at: Some(1),
    }
}

async fn delete(app: &axum::Router, id: SubscribeId) -> Value {
    let response = delete_response(app, id).await;
    assert_eq!(response.status(), StatusCode::OK);
    json_data(response).await
}

async fn delete_response(app: &axum::Router, id: SubscribeId) -> axum::response::Response {
    app.clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/subscriptions/{id}?delete_torrents=true"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap()
}

fn id(value: &Value) -> SubscribeId {
    value["id"].as_str().unwrap().parse().unwrap()
}

fn duplicate(store: &Store, original: SubscribeId) -> domain::Subscribe {
    let mut subscribe = store.get_subscribe(original).unwrap().unwrap();
    subscribe.id = SubscribeId::new();
    store.insert_subscribe(&subscribe).unwrap();
    subscribe
}

#[tokio::test]
async fn imported_pending_cleanup_uses_exact_persisted_downloader_route() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, store, downloads) = setup(&tmp);
    let (url, calls) = fixture::transmission_fixture();
    let route = DownloaderId::new();
    store
        .insert_downloader(&api::store::DownloaderRow {
            id: route,
            name: "persisted route".into(),
            kind: "transmission".into(),
            url,
            username: None,
            password: None,
            category: None,
            path_maps: Vec::new(),
            is_default: false,
            enabled: false,
        })
        .unwrap();
    let subscribe = id(&create_subscribe(&app, "search").await);
    let owned = pending(
        "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
        Some(route),
    );
    store
        .record_pending_submission(subscribe, 80, &owned)
        .unwrap();
    store
        .mark_pending_imported(subscribe, &[owned.torrent.enclosure])
        .unwrap();
    let unrelated = tmp.path().join("unrelated.mkv");
    std::fs::write(&unrelated, b"keep").unwrap();
    downloads
        .files
        .lock()
        .insert("unrelated".into(), unrelated.clone());
    assert_eq!(delete(&app, subscribe).await["removed_from_client"], 1);
    assert!(
        calls
            .lock()
            .iter()
            .any(|call| call.contains("torrent-remove"))
    );
    assert!(downloads.removed.lock().is_empty());
    assert_eq!(*downloads.scans.lock(), 0);
    assert!(unrelated.exists());
}

#[tokio::test]
async fn missing_pending_never_scans_or_removes_same_media_downloads() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, store, downloads) = setup(&tmp);
    let subscribe = id(&create_subscribe(&app, "search").await);
    let other = duplicate(&store, subscribe);
    let unrelated = pending("https://pt.example/unrelated", None);
    store
        .record_pending_submission(other.id, 80, &unrelated)
        .unwrap();
    let file = tmp.path().join("unrelated.mkv");
    std::fs::write(&file, b"keep").unwrap();
    downloads
        .files
        .lock()
        .insert(unrelated.torrent.enclosure, file.clone());
    assert_eq!(delete(&app, subscribe).await["removed_from_client"], 0);
    assert_eq!(*downloads.scans.lock(), 0);
    assert!(downloads.removed.lock().is_empty());
    assert!(file.exists());
    assert_eq!(store.load_pending(other.id).unwrap().len(), 1);
}

#[tokio::test]
async fn cleanup_removes_only_owned_identity_not_another_same_media_task() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, store, downloads) = setup(&tmp);
    let subscribe = id(&create_subscribe(&app, "search").await);
    let other = duplicate(&store, subscribe);
    for (id, enclosure) in [
        (
            subscribe,
            "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        (
            other.id,
            "magnet:?xt=urn:btih:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ),
    ] {
        let task = pending(enclosure, None);
        store.record_pending_submission(id, 80, &task).unwrap();
        let file = tmp.path().join(format!("{enclosure}.mkv"));
        std::fs::write(&file, b"fixture").unwrap();
        downloads.files.lock().insert(enclosure.into(), file);
    }
    assert_eq!(delete(&app, subscribe).await["removed_from_client"], 1);
    assert_eq!(
        *downloads.removed.lock(),
        vec!["magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]
    );
    assert!(
        !tmp.path()
            .join("magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.mkv")
            .exists()
    );
    assert!(
        tmp.path()
            .join("magnet:?xt=urn:btih:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.mkv")
            .exists()
    );
    assert_eq!(store.load_pending(other.id).unwrap().len(), 1);
}

async fn shared_reference_survives(imported: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let (app, store, downloads) = setup(&tmp);
    let subscribe = id(&create_subscribe(&app, "search").await);
    let other = duplicate(&store, subscribe);
    let shared = pending(
        "magnet:?xt=urn:btih:cccccccccccccccccccccccccccccccccccccccc",
        None,
    );
    for id in [subscribe, other.id] {
        store.record_pending_submission(id, 80, &shared).unwrap();
    }
    if imported {
        store
            .mark_pending_imported(other.id, &[shared.torrent.enclosure.clone()])
            .unwrap();
    }
    let file = tmp.path().join("shared.mkv");
    std::fs::write(&file, b"keep").unwrap();
    downloads
        .files
        .lock()
        .insert(shared.torrent.enclosure, file.clone());
    assert_eq!(delete(&app, subscribe).await["removed_from_client"], 0);
    assert!(downloads.removed.lock().is_empty());
    assert!(file.exists());
    let state = if imported { "imported" } else { "active" };
    assert_eq!(store.load_pending_state(other.id, state).unwrap().len(), 1);
}

#[tokio::test]
async fn cleanup_preserves_task_referenced_by_another_active_subscribe() {
    shared_reference_survives(false).await;
}

#[tokio::test]
async fn cleanup_preserves_task_referenced_by_another_imported_subscribe() {
    shared_reference_survives(true).await;
}

#[tokio::test]
async fn cleanup_preserves_task_referenced_by_another_subscribe_via_downloader_alias() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, store, _downloads) = setup(&tmp);
    let (url, calls) = fixture::transmission_fixture();
    let route_a = DownloaderId::new();
    let route_b = DownloaderId::new();
    for (id, name) in [(route_a, "trans-a"), (route_b, "trans-b")] {
        store
            .insert_downloader(&api::store::DownloaderRow {
                id,
                name: name.into(),
                kind: "transmission".into(),
                url: url.clone(),
                username: None,
                password: None,
                category: None,
                path_maps: Vec::new(),
                is_default: false,
                enabled: true,
            })
            .unwrap();
    }
    let sub_a = id(&create_subscribe(&app, "search").await);
    let sub_b = duplicate(&store, sub_a);
    let task_a = pending(
        "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
        Some(route_a),
    );
    let task_b = pending(
        "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
        Some(route_b),
    );
    store.record_pending_submission(sub_a, 80, &task_a).unwrap();
    store
        .record_pending_submission(sub_b.id, 80, &task_b)
        .unwrap();
    assert_eq!(delete(&app, sub_a).await["removed_from_client"], 0);
    assert!(!calls.lock().iter().any(|c| c.contains("torrent-remove")));
}

#[tokio::test]
async fn same_unproven_http_enclosure_must_reject_cleanup() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, store, downloads) = setup(&tmp);
    let subscribe = id(&create_subscribe(&app, "search").await);
    let other = duplicate(&store, subscribe);
    let shared = pending("https://pt.example/same-unproven", None);
    for id in [subscribe, other.id] {
        store.record_pending_submission(id, 80, &shared).unwrap();
    }
    let response = delete_response(&app, subscribe).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(downloads.removed.lock().is_empty());
}

/// A downloader whose ownership marks disappear together with the task, like
/// qBittorrent: HTTP enclosures share one task hash and lose it on removal.
#[derive(Default)]
struct LiveTaskDownloads {
    live: Mutex<HashMap<String, String>>,
    removed: Mutex<Vec<String>>,
}

impl Downloader for LiveTaskDownloads {
    fn add(&self, _: &Torrent) -> Result<(), DownloaderError> {
        Ok(())
    }

    fn completed_files(&self, _: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        Ok(Vec::new())
    }

    fn owned_identity(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        Ok(self.live.lock().get(&torrent.enclosure).cloned())
    }

    fn remove_owned(&self, torrent: &Torrent, _: bool) -> Result<(), DownloaderError> {
        let Some(hash) = self.live.lock().get(&torrent.enclosure).cloned() else {
            // Mirrors the real clients: a proven magnet that is already gone
            // is a safe no-op, while an unproven HTTP row must be refused.
            return downloader::magnet_info_hash(&torrent.enclosure)
                .map(|_| ())
                .ok_or_else(|| DownloaderError::Message("owned identity unproven".into()));
        };
        self.delete_live_hash(&hash, Some(&torrent.enclosure))
    }

    fn delete_task(&self, info_hash: &str, _: bool) -> Result<(), DownloaderError> {
        self.delete_live_hash(info_hash, None)
    }
}

impl LiveTaskDownloads {
    fn delete_live_hash(&self, hash: &str, enclosure: Option<&str>) -> Result<(), DownloaderError> {
        let mut live = self.live.lock();
        let recorded = enclosure.map(str::to_string).or_else(|| {
            live.iter()
                .find(|(_, live)| *live == hash)
                .map(|(enclosure, _)| enclosure.clone())
        });
        let had = live.values().any(|live| live == hash);
        live.retain(|_, live| live != hash);
        if let Some(recorded) = recorded.filter(|_| had) {
            self.removed.lock().push(recorded);
        }
        Ok(())
    }
}

fn live_task_setup(tmp: &tempfile::TempDir) -> (axum::Router, Store, Arc<LiveTaskDownloads>) {
    let downloads = Arc::new(LiveTaskDownloads::default());
    let state = api::ApiState::new(
        Store::open(tmp.path().join("data")).unwrap(),
        "management-secret".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloads.clone(),
        tmp.path().join("library"),
    )
    .unwrap();
    (
        router(state),
        Store::open(tmp.path().join("data")).unwrap(),
        downloads,
    )
}

#[tokio::test]
async fn same_hash_http_tasks_are_deleted_once_and_all_pending_cleared() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, store, downloads) = live_task_setup(&tmp);
    let subscribe = id(&create_subscribe(&app, "search").await);
    for enclosure in ["https://pt.example/alt-one", "https://pt.example/alt-two"] {
        downloads
            .live
            .lock()
            .insert(enclosure.into(), "shared-infohash".into());
        store
            .record_pending_submission(subscribe, 80, &pending(enclosure, None))
            .unwrap();
    }
    let value = delete(&app, subscribe).await;
    assert_eq!(value["removed_from_client"], 2);
    assert_eq!(
        downloads.removed.lock().len(),
        1,
        "both enclosures name one task: it must be deleted exactly once"
    );
    assert!(store.load_pending(subscribe).unwrap().is_empty());
}

#[tokio::test]
async fn season_cleanup_preserves_task_referenced_by_unselected_season_of_same_subscribe() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, store, downloads) = live_task_setup(&tmp);
    let subscribe = id(&create_subscribe(&app, "search").await);
    let s1_enclosure = "https://pt.example/season-one";
    let s2_enclosure = "https://pt.example/season-two";
    for enclosure in [s1_enclosure, s2_enclosure] {
        downloads
            .live
            .lock()
            .insert(enclosure.into(), "shared-season-hash".into());
    }
    let mut s1_task = pending(s1_enclosure, None);
    s1_task.torrent.title = "The.Matrix.S01E01.1080p".into();
    let mut s2_task = pending(s2_enclosure, None);
    s2_task.torrent.title = "The.Matrix.S02E01.1080p".into();
    store
        .record_pending_submission(subscribe, 80, &s1_task)
        .unwrap();
    store
        .record_pending_submission(subscribe, 80, &s2_task)
        .unwrap();

    let response = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{subscribe}/season-cleanup"),
            Some("management-secret"),
            json!({ "seasons": [1], "delete_torrents": true, "delete_library_files": false }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = json_data(response).await;
    assert_eq!(
        data["removed_from_client"], 0,
        "physical task is preserved for unselected season"
    );
    assert_eq!(
        data["pending_cleared"], 1,
        "selected season pending is cleared"
    );
    assert_eq!(
        downloads.removed.lock().len(),
        0,
        "physical delete must not be called"
    );
    let remaining = store.load_pending(subscribe).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].1.torrent.enclosure, s2_enclosure);
}

#[tokio::test]
async fn absent_magnet_cleanup_is_a_safe_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, store, downloads) = live_task_setup(&tmp);
    let subscribe = id(&create_subscribe(&app, "search").await);
    store
        .record_pending_submission(
            subscribe,
            80,
            &pending(
                "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                None,
            ),
        )
        .unwrap();
    let value = delete(&app, subscribe).await;
    assert_eq!(value["removed_from_client"], 1);
    assert!(downloads.removed.lock().is_empty());
}

#[tokio::test]
async fn production_dynamic_downloader_delegates_owned_removal_to_the_selected_client() {
    let tmp = tempfile::tempdir().unwrap();
    let (url, calls) = fixture::transmission_fixture();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .insert_downloader(&api::store::DownloaderRow {
            id: DownloaderId::new(),
            name: "default".into(),
            kind: "transmission".into(),
            url,
            username: None,
            password: None,
            category: None,
            path_maps: Vec::new(),
            is_default: true,
            enabled: true,
        })
        .unwrap();
    let client = api::DynamicDownloader::new(
        Arc::new(Mutex::new(store)),
        api::DownloaderEnv::default(),
        tmp.path(),
    );
    let torrent = pending(
        "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
        None,
    )
    .torrent;
    assert_eq!(
        client.owned_identity(&torrent).unwrap().as_deref(),
        Some("0123456789abcdef0123456789abcdef01234567"),
        "runtime proxy must forward ownership lookups to the selected client"
    );
    client.remove_owned(&torrent, true).unwrap();
    assert!(
        calls
            .lock()
            .iter()
            .any(|call| call.contains("torrent-remove")),
        "runtime proxy must forward the destructive removal to the selected client"
    );
}

#[tokio::test]
async fn unproven_http_identity_is_rejected_instead_of_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, store, downloads) = setup(&tmp);
    let subscribe = id(&create_subscribe(&app, "search").await);
    let other = duplicate(&store, subscribe);
    store
        .record_pending_submission(subscribe, 80, &pending("https://pt.example/owned", None))
        .unwrap();
    store
        .record_pending_submission(other.id, 80, &pending("https://pt.example/other", None))
        .unwrap();
    let response = delete_response(&app, subscribe).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(downloads.removed.lock().is_empty());
    assert_eq!(
        store.get_subscribe(subscribe).unwrap().unwrap().id,
        subscribe
    );
}

#[tokio::test]
async fn proven_magnet_is_not_deleted_when_same_route_http_identity_is_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, store, downloads) = setup(&tmp);
    let subscribe = id(&create_subscribe(&app, "search").await);
    let other = duplicate(&store, subscribe);
    store
        .record_pending_submission(
            subscribe,
            80,
            &pending(
                "magnet:?xt=urn:btih:dddddddddddddddddddddddddddddddddddddddd",
                None,
            ),
        )
        .unwrap();
    store
        .record_pending_submission(other.id, 80, &pending("https://pt.example/legacy", None))
        .unwrap();
    let response = delete_response(&app, subscribe).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(downloads.removed.lock().is_empty());
}

async fn library(app: &axum::Router, root: &std::path::Path) -> domain::LibraryId {
    let response = app.clone().oneshot(request(
        "POST", "/api/v1/libraries", Some("management-secret"),
        json!({"name":root.file_name().unwrap().to_str().unwrap(), "kind":"movie", "root_paths":[root]}),
    )).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json_data(response).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

async fn tick(app: &axum::Router, now: i64) {
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/jobs/tick?now={now}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

async fn transfer_until(app: &axum::Router, store: &Store, count: usize, start: i64) {
    for now in (start..start + 300).step_by(30) {
        tick(app, now).await;
        if store.list_ledger().unwrap().len() == count {
            return;
        }
    }
    assert_eq!(store.list_ledger().unwrap().len(), count);
}

#[tokio::test]
async fn same_media_source_transfers_to_two_libraries_but_is_idempotent_in_each() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, store, downloads) = setup(&tmp);
    let first_root = tmp.path().join("first");
    let second_root = tmp.path().join("second");
    let first_library = library(&app, &first_root).await;
    let second_library = library(&app, &second_root).await;
    let original = id(&create_subscribe(&app, "search").await);
    let mut first = store.get_subscribe(original).unwrap().unwrap();
    first.library_id = Some(first_library);
    store.update_subscribe(&first).unwrap();
    let mut second = duplicate(&store, original);
    second.library_id = Some(second_library);
    store.update_subscribe(&second).unwrap();
    let owned = pending("https://pt.example/shared-source", None);
    let source = tmp.path().join("The.Matrix.1999.1080p.mkv");
    std::fs::write(&source, b"fixture").unwrap();
    downloads
        .files
        .lock()
        .insert(owned.torrent.enclosure.clone(), source.clone());
    store
        .record_pending_submission(original, 80, &owned)
        .unwrap();
    transfer_until(&app, &store, 1, 1).await;
    let first_row = store.list_ledger().unwrap().remove(0);
    assert!(std::path::Path::new(&first_row.path).starts_with(&first_root));
    store
        .record_pending_submission(second.id, 80, &owned)
        .unwrap();
    transfer_until(&app, &store, 2, 301).await;
    let rows = store.list_ledger().unwrap();
    assert!(
        rows.iter()
            .any(|row| std::path::Path::new(&row.path).starts_with(&second_root))
    );
    let repeated = duplicate(&store, original);
    store
        .record_pending_submission(repeated.id, 80, &owned)
        .unwrap();
    for now in [601, 631, 661] {
        tick(&app, now).await;
    }
    assert_eq!(store.list_ledger().unwrap().len(), 2);
    assert_eq!(
        store.ledger_by_path(&first_row.path).unwrap().unwrap().id,
        first_row.id
    );
    assert!(
        rows.iter()
            .all(|row| std::path::Path::new(&row.path).exists())
    );
    assert!(source.exists());
}
