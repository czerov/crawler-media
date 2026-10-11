//! 两套密码登录协议共用 offlock 服务的运行时回归。
//!
//! 注入阻塞 verify：闭包进入后主测试线程必须能立刻拿到 Store 锁；阻塞期间
//! 改密 / 禁用 / 删除用户，release 后登录必须 401 且不泄露任何会话凭据。
//! 另测并发上限触发 503，且 permit 直到 CAS 结束才释放。

use std::path::Path;
use std::sync::{Arc, Condvar, Mutex as StdMutex};

use api::password_login::{PasswordEngine, PasswordLogin};
use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use domain::{User, UserId, UserRole};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::json;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

#[derive(Clone, Copy, Debug)]
enum Protocol {
    Rest,
    Jellyfin,
}

#[derive(Clone, Copy, Debug)]
enum Mutation {
    ChangePassword,
    Disable,
    Delete,
}

/// 让 verify 阻塞在 spawn_blocking 闭包内，直到测试线程显式 release。
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

struct BlockingEngine {
    gate: Arc<Gate>,
}

impl PasswordEngine for BlockingEngine {
    fn verify(&self, _password: &str, _stored: &str) -> bool {
        self.gate.enter_and_wait();
        true
    }

    fn hash(&self, password: &str) -> Result<String, String> {
        store::password::hash_password(password)
    }
}

fn setup(root: &Path, gate: Arc<Gate>, concurrency: usize) -> (ApiState, axum::Router, UserId) {
    let store = Store::open(root.join("data")).unwrap();
    let member = User {
        id: UserId::new(),
        login: "member".into(),
        enabled: true,
        role: UserRole::Member,
    };
    store
        .insert_user_with_password(&member, "original-secret")
        .unwrap();
    let state = ApiState::new(
        store,
        "admin-token".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
    .with_password_login(Arc::new(PasswordLogin::new(
        Arc::new(BlockingEngine { gate }),
        concurrency,
    )));
    let app = router(state.clone());
    (state, app, member.id)
}

fn login_request(protocol: Protocol, username: &str, password: &str) -> Request<Body> {
    let (uri, payload) = match protocol {
        Protocol::Rest => (
            "/api/v1/auth/login",
            json!({ "username": username, "password": password }),
        ),
        Protocol::Jellyfin => (
            "/Users/AuthenticateByName",
            json!({ "Username": username, "Pw": password }),
        ),
    };
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap()
}

fn apply_mutation(store: &mut Store, user_id: UserId, mutation: Mutation) {
    match mutation {
        Mutation::ChangePassword => store.set_password(user_id, "changed-secret").unwrap(),
        Mutation::Disable => {
            let mut user = store.get_user(user_id).unwrap().unwrap();
            user.enabled = false;
            store.save_user(&user).unwrap();
        }
        Mutation::Delete => assert!(store.delete_user(user_id).unwrap()),
    }
}

/// 阻塞 verify 期间改状态：登录必须被 finalizer 拒绝，且不签发 cookie/token。
async fn assert_blocked_login_rejected(protocol: Protocol, mutation: Mutation) {
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::new());
    let (state, app, member) = setup(tmp.path(), gate.clone(), 2);

    let handle = tokio::spawn({
        let app = app.clone();
        async move {
            app.oneshot(login_request(protocol, "member", "original-secret"))
                .await
                .unwrap()
        }
    });

    gate.wait_entered();
    {
        let store = state.store();
        let mut guard = store
            .try_lock()
            .expect("Store lock must stay free while the Argon2 verify blocks");
        apply_mutation(&mut guard, member, mutation);
    }
    gate.release();

    let response = handle.await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "{protocol:?} login must be rejected after {mutation:?}"
    );
    assert!(
        response.headers().get(header::SET_COOKIE).is_none(),
        "rejected login must not set a session cookie"
    );
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        !text.contains("AccessToken"),
        "rejected login leaked AccessToken: {text}"
    );
    assert!(
        !text.contains("\"token\""),
        "rejected login leaked a token: {text}"
    );
    assert!(
        state
            .store()
            .lock()
            .latest_token_for_user(member)
            .unwrap()
            .is_none(),
        "no session may be issued for a stale snapshot"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rest_changed_password_during_blocked_verify_is_rejected() {
    assert_blocked_login_rejected(Protocol::Rest, Mutation::ChangePassword).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rest_disabled_user_during_blocked_verify_is_rejected() {
    assert_blocked_login_rejected(Protocol::Rest, Mutation::Disable).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rest_deleted_user_during_blocked_verify_is_rejected() {
    assert_blocked_login_rejected(Protocol::Rest, Mutation::Delete).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellyfin_changed_password_during_blocked_verify_is_rejected() {
    assert_blocked_login_rejected(Protocol::Jellyfin, Mutation::ChangePassword).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellyfin_disabled_user_during_blocked_verify_is_rejected() {
    assert_blocked_login_rejected(Protocol::Jellyfin, Mutation::Disable).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellyfin_deleted_user_during_blocked_verify_is_rejected() {
    assert_blocked_login_rejected(Protocol::Jellyfin, Mutation::Delete).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrency_limit_returns_503_and_holds_permit_until_finalize() {
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::new());
    let (_state, app, _member) = setup(tmp.path(), gate.clone(), 1);

    let first = tokio::spawn({
        let app = app.clone();
        async move {
            app.oneshot(login_request(Protocol::Rest, "member", "original-secret"))
                .await
                .unwrap()
        }
    });
    gate.wait_entered();

    let busy = app
        .clone()
        .oneshot(login_request(Protocol::Rest, "member", "original-secret"))
        .await
        .unwrap();
    assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(busy.headers().get(header::RETRY_AFTER).unwrap(), "1");

    // permit 仍在阻塞闭包内：第三个请求同样被拒绝。
    let still_busy = app
        .clone()
        .oneshot(login_request(Protocol::Rest, "member", "original-secret"))
        .await
        .unwrap();
    assert_eq!(still_busy.status(), StatusCode::SERVICE_UNAVAILABLE);

    gate.release();
    let first = first.await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let after = app
        .clone()
        .oneshot(login_request(Protocol::Rest, "member", "original-secret"))
        .await
        .unwrap();
    assert_eq!(after.status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellyfin_busy_maps_to_503_not_401() {
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::new());
    let (_state, app, _member) = setup(tmp.path(), gate.clone(), 1);

    let first = tokio::spawn({
        let app = app.clone();
        async move {
            app.oneshot(login_request(
                Protocol::Jellyfin,
                "member",
                "original-secret",
            ))
            .await
            .unwrap()
        }
    });
    gate.wait_entered();

    let busy = app
        .clone()
        .oneshot(login_request(
            Protocol::Jellyfin,
            "member",
            "original-secret",
        ))
        .await
        .unwrap();
    assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_ne!(busy.status(), StatusCode::UNAUTHORIZED);

    gate.release();
    assert_eq!(first.await.unwrap().status(), StatusCode::OK);
}
