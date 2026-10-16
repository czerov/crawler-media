use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;

use api::{ApiState, ChosenDownloader, DownloaderEnv, Store, choose_downloader, router};
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ))
}

async fn post_dl(app: &axum::Router, body: Value) -> Value {
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/downloaders",
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED, "{response:?}");
    json_data(response).await
}

#[tokio::test]
async fn two_downloaders_one_default_and_list_omits_password() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let first = post_dl(
        &app,
        json!({
            "name": "qb-a",
            "kind": "qbittorrent",
            "url": "http://qb-a:8080",
            "username": "admin",
            "password": "secret-a",
            "is_default": true
        }),
    )
    .await;
    let second = post_dl(
        &app,
        json!({
            "name": "qb-b",
            "kind": "qbittorrent",
            "url": "http://qb-b:8080",
            "username": "admin",
            "password": "secret-b",
            "is_default": true
        }),
    )
    .await;
    assert_ne!(first["id"], second["id"]);

    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/downloaders",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let rows = json_data(listed).await;
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let a = rows.iter().find(|row| row["name"] == "qb-a").unwrap();
    let b = rows.iter().find(|row| row["name"] == "qb-b").unwrap();
    assert_eq!(a["is_default"], false);
    assert_eq!(b["is_default"], true);
    assert!(a.get("password").is_none());
    assert!(b.get("password").is_none());

    let store = Store::open(tmp.path().join("data")).unwrap();
    let ChosenDownloader::Qbittorrent(cfg) =
        choose_downloader(&store, &DownloaderEnv::default()).unwrap()
    else {
        panic!("default row");
    };
    assert_eq!(cfg.url, "http://qb-b:8080");
    assert_eq!(cfg.password, "secret-b");
}

async fn assert_default_states(app: axum::Router) {
    let listed = json_data(
        app.oneshot(request(
            "GET",
            "/api/v1/downloaders",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    let a = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "qb-a")
        .unwrap();
    let b = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "qb-b")
        .unwrap();
    assert_eq!(a["is_default"], true);
    assert_eq!(b["is_default"], false);
}

#[tokio::test]
async fn patch_makes_existing_downloader_the_default() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let first = post_dl(
        &app,
        json!({
            "name": "qb-a",
            "kind": "qbittorrent",
            "url": "http://qb-a:8080",
            "username": "admin",
            "password": "a",
            "is_default": true
        }),
    )
    .await;
    post_dl(
        &app,
        json!({
            "name": "qb-b",
            "kind": "qbittorrent",
            "url": "http://qb-b:8080",
            "username": "admin",
            "password": "b",
            "is_default": false
        }),
    )
    .await;
    let id = first["id"].as_str().unwrap();
    let patched = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/downloaders/{id}"),
            Some("management-secret"),
            json!({ "is_default": true }),
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    assert_default_states(app).await;
}

// ---------------------------------------------------------------------------
// Subscription-delivered tasks addressed by the task-center id
// ---------------------------------------------------------------------------

use domain::{
    Coverage, FetchMode, Filter, FilterAtom, FilterId, MediaId, SiteId, Subscribe, SubscribeId,
    Torrent, UserId,
};

/// Seed one subscribe plus one in-flight pending download, then hand back the
/// task-center id for that delivery (`{subscribe_id}:{enclosure}`).
fn seed_pending(root: &std::path::Path, enclosure: &str) -> String {
    seed_pending_at(root, enclosure, 1)
}

fn seed_subscribe_for_pending(store: &Store) -> Subscribe {
    let filter = Filter {
        id: FilterId::new(),
        name: "f".into(),
        atoms: vec![FilterAtom {
            priority: 10,
            rule: domain::AtomRule::Resolution("1080p".into()),
            exclude: false,
        }],
        keep_old_versions: false,
    };
    store.insert_filter(&filter).unwrap();
    let subscribe = Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: MediaId::new(),
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: filter.id,
        wash_cut: false,
        wash_cut_filter_id: None,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
        keep_old_versions: false,
    };
    store.insert_subscribe(&subscribe).unwrap();
    subscribe
}

/// Same as [`seed_pending`] but with a caller-chosen `submitted_at` (unix secs).
fn seed_pending_at(root: &std::path::Path, enclosure: &str, submitted_at: i64) -> String {
    let store = Store::open(root.join("data")).unwrap();
    let subscribe = seed_subscribe_for_pending(&store);
    let torrent = Torrent {
        id: None,
        site_id: SiteId::new(),
        title: "Movie.2026.1080p".into(),
        enclosure: enclosure.into(),
        size_bytes: Some(100),
        seeders: Some(1),
        free: false,
        hr: false,
        imdb_id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    };
    store
        .merge_pending(
            subscribe.id,
            &[(
                80,
                api::store::PendingDownload {
                    submitted_at: Some(submitted_at),
                    torrent,
                    release_override: None,
                    downloader_id: None,
                },
            )],
        )
        .unwrap();
    format!("{}:{enclosure}", subscribe.id)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

async fn list_tasks_json(app: &axum::Router) -> Value {
    json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/downloaders/tasks",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await
}

fn task_items(body: &Value) -> Vec<Value> {
    body.get("data")
        .and_then(|data| data.get("items"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn remove_task_drops_the_pending_row_and_asks_the_client_to_remove() {
    let tmp = tempfile::tempdir().unwrap();
    let enclosure = "https://pt.example/download.php?id=1";
    let task_id = seed_pending(tmp.path(), enclosure);
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader.clone(),
    ));

    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/downloaders/tasks/remove",
            Some("management-secret"),
            json!({ "task_id": task_id, "delete_files": true }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{response:?}");
    let body = json_body(response).await;
    let data = body.get("data").unwrap_or(&body);
    assert_eq!(data["delete_files"], true);

    assert_eq!(
        downloader.removed(),
        vec!["Movie.2026.1080p".to_string()],
        "删除必须真的下发到下载器"
    );
    let store = Store::open(tmp.path().join("data")).unwrap();
    assert!(
        store.list_all_pending().unwrap().is_empty(),
        "pending 行应被移除，否则下一轮会继续把用户带回同一个坏种"
    );
}

#[tokio::test]
async fn replace_task_queues_a_real_search_job_excluding_the_stalled_source() {
    let tmp = tempfile::tempdir().unwrap();
    let enclosure = "https://pt.example/download.php?id=1";
    let task_id = seed_pending(tmp.path(), enclosure);
    let app = app(&tmp);

    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/downloaders/tasks/replace",
            Some("management-secret"),
            json!({ "task_id": task_id }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{response:?}");
    let body = json_body(response).await;
    let data = body.get("data").unwrap_or(&body);
    let job_id = data["job_id"].as_str().expect("job_id");
    assert!(!job_id.is_empty());

    // The queued job carries the exclusion, and the stalled task is untouched.
    let queue = jobs::Queue::open(tmp.path().join("data").join("jobs.db")).unwrap();
    let job = queue
        .get(job_id.parse().unwrap())
        .unwrap()
        .expect("queued replacement job");
    assert_eq!(job.kind, jobs::JobKind::SubscribeSearch);
    assert!(
        job.payload.contains(enclosure),
        "换源搜索必须排除卡住的那个源: {}",
        job.payload
    );
    let store = Store::open(tmp.path().join("data")).unwrap();
    assert_eq!(
        store.list_all_pending().unwrap().len(),
        1,
        "换源期间旧任务必须保留（直到新源产生真实进度）"
    );
}

#[tokio::test]
async fn task_endpoints_reject_an_unknown_task_id() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let unknown = format!("{}:magnet:deadbeef", SubscribeId::new());

    for path in [
        "/api/v1/downloaders/tasks/remove",
        "/api/v1/downloaders/tasks/replace",
    ] {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                path,
                Some("management-secret"),
                json!({ "task_id": unknown }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn downloader_enabled_flag_round_trips_through_patch() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let created = post_dl(
        &app,
        json!({
            "name": "qb-a",
            "kind": "qbittorrent",
            "url": "http://qb-a:8080",
            "username": "admin",
            "password": "secret-a",
            "is_default": true
        }),
    )
    .await;
    assert_eq!(created["enabled"], true, "新建的下载器默认启用");
    let id = created["id"].as_str().unwrap().to_string();

    let response = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/downloaders/{id}"),
            Some("management-secret"),
            json!({ "name": "qb-a", "enabled": false }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{response:?}");
    assert_eq!(json_data(response).await["enabled"], false);
}

#[tokio::test]
async fn list_tasks_reports_recent_absent_task_as_missing_not_queued() {
    let tmp = tempfile::tempdir().unwrap();
    let enclosure = "https://pt.example/download.php?id=1";
    seed_pending_at(tmp.path(), enclosure, now_secs());
    let app = app(&tmp);

    // 下载器（MemoryDownloader）可达、快照为空 → 种子不在客户端里。宽限期内的
    // 任务必须如实上报 missing（可能刚落地、留窗口给用户换源/移除），而不是
    // 虚构「排队中」——后者会触发「活动任务位已满」的误报。
    let listed = list_tasks_json(&app).await;
    let items = task_items(&listed);
    assert_eq!(items.len(), 1, "宽限期内在途 pending 应出现在任务中心");
    assert_eq!(
        items[0]["state"], "missing",
        "客户端可达但无此种子时不能伪装成排队中"
    );

    // 宽限期内的行不能被自动清理（提交时间还不够老）。
    let store = Store::open(tmp.path().join("data")).unwrap();
    assert_eq!(store.list_all_pending().unwrap().len(), 1);
}

#[tokio::test]
async fn list_tasks_does_not_bind_pending_to_same_size_different_title() {
    let tmp = tempfile::tempdir().unwrap();
    let enclosure = "https://pt.example/download.php?id=same-size";
    seed_pending_at(tmp.path(), enclosure, now_secs());
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    downloader.push_snapshot(downloader::TaskSnapshot {
        tag: downloader::TASK_TAG.into(),
        name: "Once.Upon.a.Time.2024.1080p".into(),
        progress: 0.42,
        state: "downloading".into(),
        size_bytes: 100,
        downloaded_bytes: 42,
        uploaded_bytes: 0,
        download_speed: 1,
        upload_speed: 0,
        info_hash: "bbbb".into(),
        owned_tags: vec![],
    });
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader,
    ));

    let listed = list_tasks_json(&app).await;
    let items = task_items(&listed);
    assert_eq!(items.len(), 1, "pending A must stay visible");
    assert_eq!(
        items[0]["state"], "missing",
        "same-size unrelated snapshot must not bind to Movie.2026 pending"
    );
    assert_ne!(items[0]["progress"].as_f64().unwrap_or(0.0), 0.42);
}

#[tokio::test]
async fn list_tasks_keeps_stale_http_pending_when_client_list_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let enclosure = "https://pt.example/download.php?id=1";
    seed_pending_at(tmp.path(), enclosure, now_secs() - 30 * 60);
    let app = app(&tmp);

    // Review M-2：客户端返回空列表（刚重启、连错实例）不能证明 HTTP 种子已消失，
    // pending 必须保留；有归属标签可见时的自动清理由 subscription_e2e d_s1 覆盖。
    let listed = list_tasks_json(&app).await;
    assert_eq!(task_items(&listed).len(), 1, "{listed}");
    let store = Store::open(tmp.path().join("data")).unwrap();
    assert_eq!(store.list_all_pending().unwrap().len(), 1);
}

#[tokio::test]
async fn list_tasks_prunes_orphan_pending_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let enclosure = "https://pt.example/download.php?id=1";
    let task_id = seed_pending_at(tmp.path(), enclosure, now_secs());
    let app = app(&tmp);

    // 删除订阅后，残留的孤儿 pending 必须在下次读取时被清理——没有任何流程还能
    // 引用它们（搜索/转存都按订阅走），任务中心应随之清空。孤儿清理与宽限期
    // 无关：订阅都没了，行就是纯垃圾。
    let store = Store::open(tmp.path().join("data")).unwrap();
    let subscribe_id: SubscribeId = task_id.split(':').next().unwrap().parse().unwrap();
    store.delete_subscribe(subscribe_id).unwrap();

    let listed = list_tasks_json(&app).await;
    assert!(
        task_items(&listed).is_empty(),
        "订阅已删的孤儿任务不应再出现在任务中心"
    );
    assert!(
        store.list_all_pending().unwrap().is_empty(),
        "孤儿 pending 行应被清理"
    );
}

struct UnsupportedSnapshotDownloader;
impl downloader::Downloader for UnsupportedSnapshotDownloader {
    fn add(&self, _torrent: &domain::Torrent) -> Result<(), downloader::DownloaderError> {
        Ok(())
    }
    fn completed_files(
        &self,
        _torrent: &domain::Torrent,
    ) -> Result<Vec<std::path::PathBuf>, downloader::DownloaderError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn list_tasks_does_not_prune_stale_pending_when_snapshots_unsupported() {
    let tmp = tempfile::tempdir().unwrap();
    let enclosure = "https://pt.example/download.php?id=unsupported";
    seed_pending_at(tmp.path(), enclosure, now_secs() - 30 * 60);

    let state = ApiState::new(
        Store::open(tmp.path().join("data")).unwrap(),
        "management-secret".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(UnsupportedSnapshotDownloader),
        tmp.path().join("library"),
    )
    .unwrap();
    let app = router(state);

    let listed = list_tasks_json(&app).await;
    let items = task_items(&listed);
    assert_eq!(items.len(), 1, "不支持快照枚举的下载器其任务必须保持可见");

    let store = Store::open(tmp.path().join("data")).unwrap();
    assert_eq!(
        store.list_all_pending().unwrap().len(),
        1,
        "下载器无法提供快照时绝不能擅自清理任何 pending 行"
    );
}

fn serve_fake_qbit(deletes: Arc<Mutex<Vec<String>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for incoming in listener.incoming() {
            let mut stream = incoming.unwrap();
            let mut bytes = Vec::new();
            let mut expected = None;
            loop {
                let mut chunk = [0; 4096];
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..n]);
                if expected.is_none() {
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        expected = Some(end + 4 + length);
                    }
                }
                if expected.is_some_and(|len| bytes.len() >= len) {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&bytes).into_owned();
            if request.contains("/api/v2/torrents/delete") {
                deletes.lock().push(request.clone());
            }
            let extra = if request.contains("auth/login") {
                "Set-Cookie: SID=test; HttpOnly\r\n"
            } else {
                ""
            };
            let body = "Ok.";
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn hash_task_operations_route_to_the_named_downloader() {
    let tmp = tempfile::tempdir().unwrap();
    let default_deletes = Arc::new(Mutex::new(Vec::new()));
    let other_deletes = Arc::new(Mutex::new(Vec::new()));
    let default_url = serve_fake_qbit(default_deletes.clone());
    let other_url = serve_fake_qbit(other_deletes.clone());
    let default_client = Arc::new(
        downloader::QbitDownloader::connect(downloader::QbitConfig {
            url: default_url.clone(),
            username: "admin".into(),
            password: "pass".into(),
            category: None,
            path_maps: vec![],
        })
        .unwrap(),
    );
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        default_client,
    ));
    let default_row = post_dl(
        &app,
        json!({
            "name": "qb-default",
            "kind": "qbittorrent",
            "url": default_url,
            "username": "admin",
            "password": "pass",
            "is_default": true
        }),
    )
    .await;
    let other_row = post_dl(
        &app,
        json!({
            "name": "qb-other",
            "kind": "qbittorrent",
            "url": other_url,
            "username": "admin",
            "password": "pass"
        }),
    )
    .await;
    let other_id = other_row["id"].as_str().unwrap();
    let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let response = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/downloaders/tasks/{hash}?delete_files=true&downloader_id={other_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{response:?}");
    assert!(
        default_deletes.lock().is_empty(),
        "默认下载器不应收到另一台的 hash 删除: {}",
        default_row["id"]
    );
    assert_eq!(other_deletes.lock().len(), 1);
}

#[tokio::test]
async fn untagged_reachable_snapshot_does_not_prune_stale_pending() {
    let tmp = tempfile::tempdir().unwrap();
    let enclosure = "https://pt.example/download.php?id=untagged";
    seed_pending_at(tmp.path(), enclosure, now_secs() - 30 * 60);
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    downloader.push_snapshot(downloader::TaskSnapshot {
        tag: String::new(),
        name: "Movie.2026.1080p".into(),
        progress: 0.4,
        state: "downloading".into(),
        size_bytes: 100,
        downloaded_bytes: 40,
        uploaded_bytes: 0,
        download_speed: 1,
        upload_speed: 0,
        info_hash: "cccccccccccccccccccccccccccccccccccccccc".into(),
        owned_tags: vec![],
    });
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader,
    ));
    let listed = list_tasks_json(&app).await;
    let items = task_items(&listed);
    assert_eq!(items.len(), 1, "无标签快照不能证明 pending 已消失");
    assert_eq!(items[0]["state"], "missing");
    let store = Store::open(tmp.path().join("data")).unwrap();
    assert_eq!(
        store.list_all_pending().unwrap().len(),
        1,
        "不能证明归属时绝不能清理超过宽限期的 pending"
    );
}

#[tokio::test]
async fn d_l4_deleting_a_downloader_orphans_its_pending() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let created = post_dl(
        &app,
        json!({
            "name": "qb-gone",
            "kind": "qbittorrent",
            "url": "http://qb-gone:8080",
            "username": "admin",
            "password": "a",
            "is_default": false
        }),
    )
    .await;
    let dl: domain::DownloaderId = created["id"].as_str().unwrap().parse().unwrap();
    let subscribe_id = {
        let store = Store::open(tmp.path().join("data")).unwrap();
        let subscribe = seed_subscribe_for_pending(&store);
        let torrent = Torrent {
            id: None,
            site_id: SiteId::new(),
            title: "Movie.2026.1080p".into(),
            enclosure: "https://pt.example/gone".into(),
            size_bytes: Some(100),
            seeders: Some(1),
            free: false,
            hr: false,
            imdb_id: None,
            leechers: None,
            snatched: None,
            upload_time: None,
            detail_url: None,
            category: None,
            poster_url: None,
        };
        store
            .merge_pending(
                subscribe.id,
                &[(
                    80,
                    api::store::PendingDownload {
                        submitted_at: Some(1),
                        torrent,
                        release_override: None,
                        downloader_id: Some(dl),
                    },
                )],
            )
            .unwrap();
        subscribe.id
    };
    let response = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/downloaders/{dl}"),
            Some("management-secret"),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"]["orphaned_pending"], 1);
    let store = Store::open(tmp.path().join("data")).unwrap();
    assert!(store.load_pending(subscribe_id).unwrap().is_empty());
    assert_eq!(
        store
            .load_pending_state(subscribe_id, "orphan")
            .unwrap()
            .len(),
        1
    );
}
