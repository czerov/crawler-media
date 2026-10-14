//! today-arrivals 运行时回归：预测计算必须在 Store 锁之外、且在 blocking 预算内完成。
//!
//! 关键复现：catalog 的 `CatalogGet` 实现（如 `TmdbHttp`）需要同一个 Store 读取
//! `tmdb.api_key`。旧实现全程持有 Store 锁，冷缓存时 `try_lock` 失败 → 预测整体消失。

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex as StdMutex};

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{
    Coverage, FetchMode, Media, MediaId, MediaKind, Subscribe, SubscribeId, User, UserId, UserRole,
};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media::{CatalogGet, Tmdb, TmdbError};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "management-secret";
const MEMBER_TOKEN: &str = "member-token";
const ARRIVALS: &str = "/api/v1/subscriptions/today-arrivals";
const SEASON_ONE_KEY: &str = "/tv/100/season/1?language=zh-CN";

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

fn make_state(root: &Path) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        TOKEN.into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
}

fn today() -> String {
    store::rfc3339_from_secs(store::unix_now())[..10].to_string()
}

fn season_payload(date: &str) -> String {
    json!({
        "id": 1,
        "season_number": 1,
        "name": "Season 1",
        "episodes": [{ "episode_number": 1, "air_date": date }],
    })
    .to_string()
}

/// 模拟 `TmdbHttp`：必须拿到同一个 Store 才能读取配置。
fn store_api_key(store: &Mutex<Store>) -> Result<(), TmdbError> {
    for _ in 0..50 {
        if let Some(guard) = store.try_lock() {
            let key = guard
                .get_setting("tmdb.api_key")
                .map_err(|error| TmdbError::Http(error.to_string()))?;
            if key.is_none() {
                return Err(TmdbError::Http("tmdb api key not configured".into()));
            }
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    Err(TmdbError::Http("Store lock is held while arrivals resolves".into()))
}

/// 冷缓存路径：返回 body 前必须成功借用 Store。
struct LockAwareGet {
    store: Arc<Mutex<Store>>,
    body: String,
}

impl CatalogGet for LockAwareGet {
    fn get(&self, _path: &str) -> Result<String, TmdbError> {
        store_api_key(&self.store)?;
        Ok(self.body.clone())
    }
}

/// 永不走网络；只有缓存命中才可能成功。
struct NeverGet;

impl CatalogGet for NeverGet {
    fn get(&self, _path: &str) -> Result<String, TmdbError> {
        Err(TmdbError::Http("offline: no network in tests".into()))
    }
}

/// 按路径选择性成功：用于验证部分源失败不影响其余 items。
struct SelectiveGet {
    store: Arc<Mutex<Store>>,
    ok_path_marker: &'static str,
    ok_body: String,
}

impl CatalogGet for SelectiveGet {
    fn get(&self, path: &str) -> Result<String, TmdbError> {
        store_api_key(&self.store)?;
        if path.contains(self.ok_path_marker) {
            Ok(self.ok_body.clone())
        } else {
            Err(TmdbError::Http("upstream 503 for this source".into()))
        }
    }
}

/// 统计 catalog 调用次数：同一请求内同 (tmdb_id, season) 只应查询一次。
struct CountingGet {
    store: Arc<Mutex<Store>>,
    body: String,
    calls: Arc<AtomicUsize>,
}

impl CatalogGet for CountingGet {
    fn get(&self, _path: &str) -> Result<String, TmdbError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        store_api_key(&self.store)?;
        Ok(self.body.clone())
    }
}

/// 让 catalog 调用阻塞，便于观察阻塞期间 Store 锁是否空闲。
struct GatedGet {
    store: Arc<Mutex<Store>>,
    gate: Arc<Gate>,
    body: String,
}

impl CatalogGet for GatedGet {
    fn get(&self, _path: &str) -> Result<String, TmdbError> {
        self.gate.enter_and_wait();
        store_api_key(&self.store)?;
        Ok(self.body.clone())
    }
}

struct Gate {
    entered: StdMutex<bool>,
    entered_cv: Condvar,
    open: StdMutex<bool>,
    open_cv: Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            entered: StdMutex::new(false),
            entered_cv: Condvar::new(),
            open: StdMutex::new(false),
            open_cv: Condvar::new(),
        }
    }

    fn enter_and_wait(&self) {
        *self.entered.lock().unwrap() = true;
        self.entered_cv.notify_all();
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.open_cv.wait(open).unwrap();
        }
    }

    fn wait_entered(&self) {
        let mut entered = self.entered.lock().unwrap();
        while !*entered {
            entered = self.entered_cv.wait(entered).unwrap();
        }
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.open_cv.notify_all();
    }
}

/// 断言失败时也要放行阻塞 closure，避免 runtime 关闭时卡住。
struct ReleaseOnDrop(Arc<Gate>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn insert_tv_subscribe(store: &Store, user_id: UserId, title: &str, tmdb_id: &str) -> SubscribeId {
    let filter_id = store.list_filters().unwrap()[0].id;
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: title.into(),
            year: Some(2026),
            original_title: None,
            tmdb_id: Some(tmdb_id.into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let subscribe_id = SubscribeId::new();
    store
        .insert_subscribe(&Subscribe {
            id: subscribe_id,
            user_id,
            media_id,
            coverage: Coverage::Tv {
                season: 1,
                episode_from: 1,
                episode_to: Some(2),
            },
            fetch_mode: FetchMode::Search,
            filter_id,
            wash_cut: false,
            wash_cut_filter_id: None,
            keep_old_versions: false,
            full_season_pack: false,
            downloader_id: None,
            library_id: None,
            tracking_state: "active".into(),
            follow_future: false,
            search_interval_secs: 1800,
        })
        .unwrap();
    subscribe_id
}

fn seed_admin_tv(state: &ApiState, title: &str, tmdb_id: &str) -> SubscribeId {
    let store = state.store();
    let guard = store.lock();
    let user_id = guard.user_id_by_token(TOKEN).unwrap().unwrap();
    guard
        .put_setting("tmdb.api_key", "offline-never-sent")
        .unwrap();
    insert_tv_subscribe(&guard, user_id, title, tmdb_id)
}

fn seed_member_tv(state: &ApiState, title: &str, tmdb_id: &str) -> SubscribeId {
    let store = state.store();
    let guard = store.lock();
    let user_id = UserId::new();
    guard
        .insert_user_with_password(
            &User {
                id: user_id,
                login: "member".into(),
                enabled: true,
                role: UserRole::Member,
            },
            "member-secret",
        )
        .unwrap();
    guard.set_user_token(user_id, MEMBER_TOKEN).unwrap();
    insert_tv_subscribe(&guard, user_id, title, tmdb_id)
}

fn arrivals_request(token: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(ARRIVALS)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

async fn json_data(response: axum::response::Response) -> Value {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["ok"], true, "status={status} body={body}");
    body.get("data").cloned().unwrap_or(Value::Null)
}

fn has_wanted(rows: &Value, subscription_id: SubscribeId) -> bool {
    rows.as_array().is_some_and(|rows| {
        rows.iter().any(|row| {
            row["status"] == "wanted" && row["subscription_id"] == subscription_id.to_string()
        })
    })
}

#[tokio::test]
async fn arrivals_cold_catalog_can_read_store() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let subscribe = seed_admin_tv(&state, "Cold Show", "100");
    let get = LockAwareGet {
        store: state.store(),
        body: season_payload(&today()),
    };
    let tmdb = Tmdb::new(get, &tmp.path().join("data/catalog.db")).unwrap();
    let app = router(state.with_catalog(api::catalog::TmdbCatalog::new(tmdb)));

    let response = app.oneshot(arrivals_request(TOKEN)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let rows = json_data(response).await;
    assert!(
        has_wanted(&rows, subscribe),
        "cold-cache arrivals lost the forecast: {rows}"
    );
}

#[tokio::test]
async fn arrivals_fresh_cache_returns_forecast_without_network() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let subscribe = seed_admin_tv(&state, "Warm Show", "100");
    let now = store::unix_now();
    state
        .store()
        .lock()
        .put_catalog_cache(
            "tmdb",
            SEASON_ONE_KEY,
            &season_payload(&today()),
            now,
            Some(now + 3600),
        )
        .unwrap();
    let tmdb = Tmdb::new(NeverGet, &tmp.path().join("data/catalog.db")).unwrap();
    let app = router(state.with_catalog(api::catalog::TmdbCatalog::new(tmdb)));

    let response = app.oneshot(arrivals_request(TOKEN)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let rows = json_data(response).await;
    assert!(
        has_wanted(&rows, subscribe),
        "fresh cache must serve the forecast offline: {rows}"
    );
}

#[tokio::test]
async fn arrivals_expired_cache_falls_back_to_stale_on_network_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let subscribe = seed_admin_tv(&state, "Stale Show", "100");
    let now = store::unix_now();
    state
        .store()
        .lock()
        .put_catalog_cache(
            "tmdb",
            SEASON_ONE_KEY,
            &season_payload(&today()),
            now - 7200,
            Some(now - 60),
        )
        .unwrap();
    let tmdb = Tmdb::new(NeverGet, &tmp.path().join("data/catalog.db")).unwrap();
    let app = router(state.with_catalog(api::catalog::TmdbCatalog::new(tmdb)));

    let response = app.oneshot(arrivals_request(TOKEN)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let rows = json_data(response).await;
    assert!(
        has_wanted(&rows, subscribe),
        "expired cache plus network failure must fall back to stale: {rows}"
    );
}

#[tokio::test]
async fn arrivals_isolate_users() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let admin_subscribe = seed_admin_tv(&state, "Admin Show", "100");
    let member_subscribe = seed_member_tv(&state, "Member Show", "100");
    let get = LockAwareGet {
        store: state.store(),
        body: season_payload(&today()),
    };
    let tmdb = Tmdb::new(get, &tmp.path().join("data/catalog.db")).unwrap();
    let app = router(state.with_catalog(api::catalog::TmdbCatalog::new(tmdb)));

    let admin_rows = json_data(app.clone().oneshot(arrivals_request(TOKEN)).await.unwrap()).await;
    assert!(has_wanted(&admin_rows, admin_subscribe));
    assert!(
        has_wanted(&admin_rows, member_subscribe),
        "admin sees every subscribe: {admin_rows}"
    );

    let member_rows = json_data(app.oneshot(arrivals_request(MEMBER_TOKEN)).await.unwrap()).await;
    let member_rows = member_rows.as_array().unwrap();
    assert_eq!(
        member_rows.len(),
        1,
        "member must only see own rows: {member_rows:?}"
    );
    assert_eq!(
        member_rows[0]["subscription_id"],
        member_subscribe.to_string()
    );
}

#[tokio::test]
async fn arrivals_partial_source_failure_keeps_other_items() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let ok_subscribe = seed_admin_tv(&state, "Reachable Show", "100");
    let failing_subscribe = seed_admin_tv(&state, "Unreachable Show", "200");
    let get = SelectiveGet {
        store: state.store(),
        ok_path_marker: "/tv/100/",
        ok_body: season_payload(&today()),
    };
    let tmdb = Tmdb::new(get, &tmp.path().join("data/catalog.db")).unwrap();
    let app = router(state.with_catalog(api::catalog::TmdbCatalog::new(tmdb)));

    let response = app.oneshot(arrivals_request(TOKEN)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let rows = json_data(response).await;
    assert!(
        has_wanted(&rows, ok_subscribe),
        "reachable source must keep its forecast: {rows}"
    );
    assert!(
        !has_wanted(&rows, failing_subscribe),
        "failing source must not fabricate a forecast: {rows}"
    );
}

#[tokio::test]
async fn arrivals_query_each_season_once_per_request() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let first = seed_admin_tv(&state, "Show A", "100");
    let second = seed_admin_tv(&state, "Show B", "100");
    let calls = Arc::new(AtomicUsize::new(0));
    let get = CountingGet {
        store: state.store(),
        body: season_payload(&today()),
        calls: calls.clone(),
    };
    let tmdb = Tmdb::new(get, &tmp.path().join("data/catalog.db")).unwrap();
    let app = router(state.with_catalog(api::catalog::TmdbCatalog::new(tmdb)));

    let rows = json_data(app.oneshot(arrivals_request(TOKEN)).await.unwrap()).await;
    assert!(
        has_wanted(&rows, first) && has_wanted(&rows, second),
        "{rows}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the same (tmdb_id, season) must be queried once per request"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_catalog_call_does_not_hold_store_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    seed_admin_tv(&state, "Blocked Show", "100");
    let gate = Arc::new(Gate::new());
    let get = GatedGet {
        store: state.store(),
        gate: gate.clone(),
        body: season_payload(&today()),
    };
    let tmdb = Tmdb::new(get, &tmp.path().join("data/catalog.db")).unwrap();
    let app = router(
        state
            .clone()
            .with_catalog(api::catalog::TmdbCatalog::new(tmdb)),
    );

    let handle = tokio::spawn({
        let app = app.clone();
        async move { app.oneshot(arrivals_request(TOKEN)).await.unwrap() }
    });
    gate.wait_entered();
    let _release = ReleaseOnDrop(gate.clone());
    {
        let store = state.store();
        assert!(
            store.try_lock().is_some(),
            "arrivals must not hold the Store lock while the catalog call blocks"
        );
    }
    gate.release();
    let response = handle.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let rows = json_data(response).await;
    assert!(
        rows.as_array().is_some_and(|rows| !rows.is_empty()),
        "{rows}"
    );
}
