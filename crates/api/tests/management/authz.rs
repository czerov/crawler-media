//! 管理员授权层与用户隔离的回归测试（audit round 2）。
//!
//! 覆盖四类历史漏洞：
//! - 成员可以调用管理员接口（旧 POST /users 创建账号等）→ 现在路由层统一 403；
//! - 订阅接口不按用户隔离（看/改/删别人的订阅）→ 现在 404；
//! - 登出不撤销会话、改密不撤销旧 token、token 永不过期 → 现在全部撤销/过期；
//! - 回收站清空忽略文件删除失败 → 现在失败保留记录。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use api::{Store, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{
    Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, User, UserId,
    UserRole,
};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use parking_lot::Mutex;
use rusqlite::params;
use serde_json::{Value, json};
use std::str::FromStr;
use tower::{Service, ServiceExt};

use super::common::{Fixtures, json_body, request, scan_and_wait, state as common_state};

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

fn authed_app(tmp: &tempfile::TempDir) -> axum::Router {
    router(common_state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ))
}

fn bearer(token: &str) -> Request<Body> {
    request("GET", "/api/v1/auth/me", Some(token), Value::Null)
}

async fn login_token(app: &axum::Router, username: &str, password: &str) -> String {
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": username, "password": password }),
        ))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "login {username} must succeed"
    );
    json_body(response).await["data"]["token"]
        .as_str()
        .unwrap()
        .to_string()
}

/// 通过 legacy /users（管理员）创建成员并登录，返回 (user_id, token)。
async fn create_member(app: &axum::Router, login: &str, password: &str) -> (String, String) {
    let admin = login_token(app, "admin", "management-secret").await;
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/users",
            Some(&admin),
            json!({ "login": login, "password": password }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let id = json_body(created).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let token = login_token(app, login, password).await;
    (id, token)
}

#[tokio::test]
async fn member_is_forbidden_on_the_admin_surface() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let (_member_id, member) = create_member(&app, "alice", "alice-pass").await;

    let admin_only = [
        (
            "POST",
            "/api/v1/users",
            json!({ "login": "x", "password": "y" }),
        ),
        ("GET", "/api/v1/users", Value::Null),
        (
            "POST",
            "/api/v1/libraries",
            json!({ "kind": "movie", "name": "n", "root_paths": ["/tmp/x"] }),
        ),
        ("PUT", "/api/v1/directory", json!({})),
        ("POST", "/api/v1/jobs/tick", json!({})),
    ];
    for (method, uri, body) in admin_only {
        let response = app
            .clone()
            .oneshot(request(method, uri, Some(&member), body))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "{method} {uri} must be admin-only"
        );
    }

    // 对照组：管理员不受影响。
    let admin = login_token(&app, "admin", "management-secret").await;
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/libraries",
            Some(&admin),
            json!({ "kind": "movie", "name": "电影", "root_paths": ["/tmp/movies"] }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn member_cannot_reach_global_library_mutations_or_legacy_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let (_member_id, member) = create_member(&app, "library-member", "member-pass").await;
    let libraries = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/libraries",
                Some(&member),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let library_id = libraries["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|library| library["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let guarded = [
        ("POST", format!("/api/v1/libraries/{library_id}/scan")),
        (
            "DELETE",
            format!("/api/v1/libraries/{library_id}/missing-rows"),
        ),
        (
            "DELETE",
            "/api/v1/catalog/cache?source=tmdb&source_id=1".into(),
        ),
        ("GET", "/api/v1/ledger".into()),
    ];
    for (method, uri) in guarded {
        let response = app
            .clone()
            .oneshot(request(method, &uri, Some(&member), Value::Null))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "{method} {uri} must be admin-only"
        );
    }
}

#[tokio::test]
async fn collections_are_private_to_their_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let (_member_id, member) = create_member(&app, "collection-member", "member-pass").await;
    let admin = login_token(&app, "admin", "management-secret").await;
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/collections",
                Some(&admin),
                json!({ "name": "管理员片单" }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let collection_id = created["data"]["id"].as_str().unwrap();
    for (method, uri, body) in [
        (
            "GET",
            format!("/api/v1/collections/{collection_id}/items"),
            Value::Null,
        ),
        (
            "PATCH",
            format!("/api/v1/collections/{collection_id}"),
            json!({ "name": "篡改" }),
        ),
        (
            "DELETE",
            format!("/api/v1/collections/{collection_id}"),
            Value::Null,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(request(method, &uri, Some(&member), body))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{method} {uri} must stay private"
        );
    }
}

async fn create_scanned_video_library(
    app: &axum::Router,
    admin: &str,
    dir: &std::path::Path,
    filename: &str,
    members: &[String],
) -> (String, String) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(filename), b"video").unwrap();
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some(admin),
                json!({
                    "kind": "video", "name": "视频库", "root_paths": [dir.display().to_string()],
                    "access_mode": "selected", "member_ids": members,
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let lib_id = created["data"]["id"].as_str().unwrap().to_string();
    scan_and_wait(&app, &lib_id).await;
    let items = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{lib_id}/items"),
                Some(admin),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let media_item_id = items["data"][0]["media_item_id"]
        .as_str()
        .unwrap()
        .to_string();
    (lib_id, media_item_id)
}

#[tokio::test]
async fn collections_hide_items_after_library_access_is_revoked() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let (member_id, member) = create_member(&app, "collection-library-member", "member-pass").await;
    let admin = login_token(&app, "admin", "management-secret").await;
    let root = tmp.path().join("restricted-videos");
    let (library_id, media_item_id) =
        create_scanned_video_library(&app, &admin, &root, "Private Movie.mkv", &[member_id]).await;

    let collection = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/collections",
                Some(&member),
                json!({ "name": "过去可见" }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let collection_id = collection["data"]["id"].as_str().unwrap();
    let added = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/collections/{collection_id}/items"),
            Some(&member),
            json!({ "media_item_id": media_item_id }),
        ))
        .await
        .unwrap();
    assert_eq!(added.status(), StatusCode::OK);

    let changed = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/libraries/{library_id}"),
            Some(&admin),
            json!({ "access_mode": "selected", "member_ids": [] }),
        ))
        .await
        .unwrap();
    assert_eq!(changed.status(), StatusCode::OK);
    let collection_items = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/collections/{collection_id}/items"),
                Some(&member),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(collection_items["data"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn collection_rejects_items_from_a_library_the_member_cannot_see() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let (_member_id, member) = create_member(&app, "collection-hidden-member", "member-pass").await;
    let admin = login_token(&app, "admin", "management-secret").await;
    let root = tmp.path().join("hidden-videos");
    let (_library_id, media_item_id) =
        create_scanned_video_library(&app, &admin, &root, "Hidden Movie.mkv", &[]).await;

    let collection = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/collections",
                Some(&member),
                json!({ "name": "不可加入" }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let collection_id = collection["data"]["id"].as_str().unwrap();
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/collections/{collection_id}/items"),
            Some(&member),
            json!({ "media_item_id": media_item_id }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

async fn assert_bob_cannot_reach_alice_sub(app: &axum::Router, bob: &str, id: &str) {
    let listed = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/subscriptions",
            Some(bob),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(json_body(listed).await["data"].as_array().unwrap().len(), 0);
    for (method, uri) in [
        ("GET", format!("/api/v1/subscriptions/{id}")),
        ("PATCH", format!("/api/v1/subscriptions/{id}")),
        ("DELETE", format!("/api/v1/subscriptions/{id}")),
        ("POST", format!("/api/v1/subscriptions/{id}/search")),
        ("POST", format!("/api/v1/subscriptions/{id}/run")),
        ("GET", format!("/api/v1/subscriptions/{id}/removal-preview")),
        ("POST", format!("/api/v1/subscriptions/{id}/upgrade-runs")),
    ] {
        let response = app
            .clone()
            .oneshot(request(method, &uri, Some(bob), json!({})))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {uri}");
    }
}

#[tokio::test]
async fn subscriptions_are_isolated_per_user() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let (_alice_id, alice) = create_member(&app, "alice", "alice-pass").await;
    let (_bob_id, bob) = create_member(&app, "bob", "bob-pass").await;

    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some(&alice),
            json!({
                "media": { "kind": "movie", "title": "The Matrix", "tmdb_id": "603" },
                "coverage": { "kind": "movie" },
                "fetch_mode": "search"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let id = json_body(created).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    assert_bob_cannot_reach_alice_sub(&app, &bob, &id).await;
    assert_sub_owner_and_admin_view(&app, &id, &alice, &bob).await;
}

async fn assert_sub_owner_and_admin_view(app: &axum::Router, id: &str, alice: &str, bob: &str) {
    let own = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/subscriptions/{id}"),
            Some(alice),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(own.status(), StatusCode::OK);
    let admin = login_token(app, "admin", "management-secret").await;
    let admin_view = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/subscriptions",
            Some(&admin),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(admin_view.status(), StatusCode::OK);
    assert_eq!(
        json_body(admin_view).await["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let arrivals = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/subscriptions/today-arrivals",
            Some(bob),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(
        json_body(arrivals).await["data"].as_array().unwrap().len(),
        0
    );
}

// ---------------------------------------------------------------------------
// up-next 用户隔离
// ---------------------------------------------------------------------------

fn tv_media() -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "九门".into(),
        year: Some(2026),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("123".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn ledger_row(media_id: MediaId, root: &Path, episode: u32) -> LedgerRow {
    LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: root
            .join(format!("E{episode:02}.mkv"))
            .display()
            .to_string(),
        season: Some(1),
        episode: Some(episode),
        resolution: Some("2160p".into()),
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::Low,
        filter_score: Some(60),
    }
}

fn setup_tv_show_and_subscribes(
    tmp: &tempfile::TempDir,
    root: &std::path::Path,
    alice_uuid: &str,
    bob_uuid: &str,
) -> MediaId {
    let media = tv_media();
    let media_id = media.id;
    let store = Store::open(tmp.path().join("data")).unwrap();
    store.insert_media(&media).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "剧集",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    for episode in 1..=3u32 {
        store
            .insert_ledger(&ledger_row(media_id, root, episode))
            .unwrap();
    }
    let subscribe_for = |user_id: UserId| domain::Subscribe {
        id: domain::SubscribeId::new(),
        user_id,
        media_id,
        coverage: domain::Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: None,
        },
        fetch_mode: domain::FetchMode::Search,
        filter_id: domain::FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };
    store
        .insert_subscribe(&subscribe_for(UserId::from_str(alice_uuid).unwrap()))
        .unwrap();
    store
        .insert_subscribe(&subscribe_for(UserId::from_str(bob_uuid).unwrap()))
        .unwrap();
    media_id
}

async fn assert_alice_and_bob_up_next(
    app: &axum::Router,
    alice: &str,
    bob: &str,
    media_id: domain::MediaId,
) {
    let marked = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/marks",
            Some(alice),
            json!({
                "media_item_id": media_id.to_string(),
                "season_number": 1,
                "episode_number": 1,
                "played": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(marked.status(), StatusCode::OK);

    let up_next_items = async |token: &str| -> Vec<Value> {
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                "/api/v1/playback/up-next",
                Some(token),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        json_body(response).await["data"]["items"]
            .as_array()
            .unwrap()
            .clone()
    };

    let alice_items = up_next_items(alice).await;
    assert_eq!(alice_items.len(), 1);
    assert_eq!(alice_items[0]["episode_number"], 2);
    assert_eq!(alice_items[0]["advanced"], true);

    let bob_items = up_next_items(bob).await;
    assert_eq!(bob_items.len(), 1);
    assert_eq!(bob_items[0]["episode_number"], 1);
    assert_eq!(bob_items[0]["advanced"], false);

    let admin = login_token(app, "admin", "management-secret").await;
    assert!(up_next_items(&admin).await.is_empty());
}

/// A 看完 E1 → A 的下一集是 E2 且 advanced；B 没播过 → 下一集 E1、不伪造
/// 播放时间；admin 没订阅 → 看不到任何人的数据。
#[tokio::test]
async fn up_next_is_isolated_per_user() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("media");
    std::fs::create_dir_all(&root).unwrap();
    let app = authed_app(&tmp);
    let (_alice_id, alice) = create_member(&app, "alice", "alice-pass").await;
    let (_bob_id, bob) = create_member(&app, "bob", "bob-pass").await;
    let uuid_of = async |token: &str| -> String {
        let me = app.clone().oneshot(bearer(token)).await.unwrap();
        json_body(me).await["data"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let alice_uuid = uuid_of(&alice).await;
    let bob_uuid = uuid_of(&bob).await;
    let media_id = setup_tv_show_and_subscribes(&tmp, &root, &alice_uuid, &bob_uuid);

    assert_alice_and_bob_up_next(&app, &alice, &bob, media_id).await;
}

// ---------------------------------------------------------------------------
// 洗版事实持久化（UpgradeLadder 同分替换必须写进 DB）
// ---------------------------------------------------------------------------

fn seed_facts_user_and_subscribe(store: &Store) -> domain::SubscribeId {
    let user = User {
        id: UserId::new(),
        login: "alice".into(),
        enabled: true,
        role: UserRole::Member,
    };
    store.insert_user(&user).unwrap();
    let media = tv_media();
    store.insert_media(&media).unwrap();
    let subscribe = domain::Subscribe {
        id: domain::SubscribeId::new(),
        user_id: user.id,
        media_id: media.id,
        coverage: domain::Coverage::Movie,
        fetch_mode: domain::FetchMode::Search,
        filter_id: domain::FilterId::new(),
        wash_cut: true,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };
    store.insert_subscribe(&subscribe).unwrap();
    subscribe.id
}

#[test]
fn facts_persist_same_score_wash_upgrade() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let sub_id = seed_facts_user_and_subscribe(&store);

    use subscribe::{QualityFact, SubscribeFacts};
    let mut facts = SubscribeFacts::default();
    facts.upsert(
        None,
        None,
        QualityFact {
            score: 80,
            path: Some("/old".into()),
        },
    );
    store.save_subscribe_facts(sub_id, &facts).unwrap();

    // chooser 批准的洗版替换：同分新路径（内存层 replace 无条件覆盖）。
    facts.replace(
        None,
        None,
        QualityFact {
            score: 80,
            path: Some("/new".into()),
        },
    );
    store.save_subscribe_facts(sub_id, &facts).unwrap();

    let loaded = store.load_subscribe_facts(sub_id).unwrap();
    assert_eq!(
        loaded.movie().unwrap().path.as_deref(),
        Some("/new"),
        "DB 必须镜像 chooser 批准的同分替换，否则永远指向已删除的旧文件"
    );

    // 更低分的批准替换（UpgradeLadder 允许）也必须持久化。
    facts.replace(
        None,
        None,
        QualityFact {
            score: 60,
            path: Some("/ladder".into()),
        },
    );
    store.save_subscribe_facts(sub_id, &facts).unwrap();
    let loaded = store.load_subscribe_facts(sub_id).unwrap();
    assert_eq!(loaded.movie().unwrap().path.as_deref(), Some("/ladder"));
    assert_eq!(loaded.movie().unwrap().score, 60);
}
