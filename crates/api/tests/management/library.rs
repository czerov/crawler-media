//! Library entity + multi-folder: seeding, create/patch/delete/default/
//! reorder, stats across roots, and scan walking every root.

use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    router(state(tmp.path(), fetcher, downloader))
}

async fn get(app: &axum::Router, uri: &str) -> Value {
    let full_uri = if uri.starts_with("/api/v1") {
        uri.to_string()
    } else {
        format!("/api/v1{uri}")
    };
    json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &full_uri,
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await
}

async fn create_library(app: &axum::Router, name: &str, kind: &str, roots: &[String]) -> String {
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": name,
                    "kind": kind,
                    "root_paths": roots,
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    created["data"]["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn libraries_scan_walks_all_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let root_a = tmp.path().join("movies-a");
    let root_b = tmp.path().join("movies-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    std::fs::write(root_a.join("The.Matrix.1999.2160p.mkv"), b"matrix").unwrap();
    std::fs::write(root_b.join("Dune.2021.2160p.mkv"), b"dune").unwrap();

    let id = create_library(
        &app,
        "电影",
        "movie",
        &[root_a.display().to_string(), root_b.display().to_string()],
    )
    .await;

    scan_and_wait(&app, &id).await;

    let body = get(&app, "/api/v1/libraries").await;
    let library = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"].as_str().unwrap() == id)
        .unwrap();
    assert_eq!(library["stats"]["file_count"], 2);
    assert!(library["stats"]["total_size_bytes"].as_i64().unwrap() > 0);

    let items = get(&app, &format!("/api/v1/libraries/{id}/items")).await;
    let titles: Vec<&str> = items["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles.len(), 2);
}

#[tokio::test]
async fn library_item_poster_url_uses_hyphenated_uuid() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("The.Matrix.1999.2160p.mkv"), b"matrix").unwrap();
    std::fs::write(root.join("poster.jpg"), b"jpeg").unwrap();
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "电影",
                    "kind": "movie",
                    "root_paths": [root.display().to_string()],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap().to_string();
    scan_and_wait(&app, &id).await;
    let items = get(&app, &format!("/api/v1/libraries/{id}/items")).await;
    let poster = items["data"][0]["poster_url"].as_str().unwrap();
    assert!(
        poster.contains('-'),
        "self-hosted poster URL must keep UUID hyphens: {poster}"
    );
    assert!(poster.starts_with("/posters/"));
}

#[tokio::test]
async fn libraries_delete_extra_but_protect_last_of_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let root = tmp.path().join("movies-x");
    let extra_id = create_library(&app, "额外电影", "movie", &[root.display().to_string()]).await;

    let deleted = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/libraries/{extra_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::ACCEPTED);
    let body = get(&app, "/api/v1/libraries").await;
    assert!(
        !body["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["id"].as_str().unwrap() == extra_id),
        "extra library removed"
    );

    let movie_id = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let protected = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/libraries/{movie_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(protected.status(), StatusCode::CONFLICT);
    let pbody = json_body(protected).await;
    assert_eq!(pbody["error"]["code"], "library.protected");
}

async fn reorder_libraries(app: &axum::Router, ids: &[String]) {
    let reordered = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/libraries/order",
            Some("management-secret"),
            json!({ "ids": ids }),
        ))
        .await
        .unwrap();
    assert_eq!(reordered.status(), StatusCode::OK);
}

#[tokio::test]
async fn libraries_set_default_and_reorder() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let root = tmp.path().join("movies-x");
    let new_id = create_library(&app, "B 库", "movie", &[root.display().to_string()]).await;

    let promoted = app
        .clone()
        .oneshot(request(
            "PUT",
            &format!("/api/v1/libraries/{new_id}/default"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(promoted.status(), StatusCode::OK);
    let body = json_body(promoted).await;
    assert_eq!(body["data"]["is_default"], true);

    let listed = get(&app, "/api/v1/libraries").await;
    let defaults: Vec<&str> = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["kind"] == "movie" && l["is_default"] == true)
        .map(|l| l["id"].as_str().unwrap())
        .collect();
    assert_eq!(defaults, vec![new_id.as_str()], "exactly one movie default");

    // Reorder: put the new library first.
    let ids: Vec<String> = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].as_str().unwrap().to_string())
        .collect();
    let mut reversed = ids.clone();
    reversed.reverse();
    reorder_libraries(&app, &reversed).await;

    let after = get(&app, "/api/v1/libraries").await;
    let order: Vec<&str> = after["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        order,
        reversed.iter().map(|s| s.as_str()).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn libraries_scope_items_to_their_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let root_a = tmp.path().join("movies-a");
    let root_b = tmp.path().join("movies-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    std::fs::write(root_a.join("The.Matrix.1999.2160p.mkv"), b"matrix").unwrap();
    std::fs::write(root_b.join("Dune.2021.2160p.mkv"), b"dune").unwrap();

    // Two movie libraries, one root each; scan both.
    let id_a = create_library(&app, "库 A", "movie", &[root_a.display().to_string()]).await;
    let id_b = create_library(&app, "库 B", "movie", &[root_b.display().to_string()]).await;
    for id in [&id_a, &id_b] {
        scan_and_wait(&app, id).await;
    }

    let items_a = get(&app, &format!("/api/v1/libraries/{id_a}/items")).await;
    let titles_a: Vec<&str> = items_a["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles_a,
        vec!["The Matrix"],
        "库 A sees only its own folder"
    );
    let items_b = get(&app, &format!("/api/v1/libraries/{id_b}/items")).await;
    let titles_b: Vec<&str> = items_b["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles_b, vec!["Dune"], "库 B sees only its own folder");
}

async fn create_custom_filter(app: &axum::Router, token: &str) -> String {
    let filter_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/rule-sets",
            Some(token),
            json!({
                "name": "4K极清规则",
                "atoms": [
                    { "kind": "resolution", "value": "2160p", "priority": 100 }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(filter_res.status(), StatusCode::CREATED);
    json_body(filter_res).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn login_admin_token(app: &axum::Router) -> String {
    let login_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": "admin", "password": "management-secret" }),
        ))
        .await
        .unwrap();
    assert_eq!(login_res.status(), StatusCode::OK);
    json_body(login_res).await["data"]["token"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn libraries_support_default_filter_association_and_subscription_inheritance() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let token = login_admin_token(&app).await;
    let custom_filter_id = create_custom_filter(&app, &token).await;

    let root = tmp.path().join("anime-4k");
    let create_lib_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/libraries",
            Some(&token),
            json!({
                "name": "动漫4K库",
                "kind": "tv",
                "root_paths": [root.display().to_string()],
                "default_filter_id": custom_filter_id,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(create_lib_res.status(), StatusCode::OK);
    let lib_id = json_body(create_lib_res).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let preview_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions/download-routing-preview",
            Some(&token),
            json!({ "kind": "tv", "library_id": lib_id }),
        ))
        .await
        .unwrap();
    assert_eq!(preview_res.status(), StatusCode::OK);
    assert_eq!(
        json_body(preview_res).await["data"]["default_filter_id"],
        custom_filter_id
    );

    let sub_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some(&token),
            json!({
                "media": { "kind": "tv", "title": "测试剧集", "tmdb_id": "99999" },
                "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 12 },
                "fetch_mode": "search",
                "library_id": lib_id
            }),
        ))
        .await
        .unwrap();
    assert_eq!(sub_res.status(), StatusCode::CREATED);
    assert_eq!(
        json_body(sub_res).await["data"]["filter_id"],
        custom_filter_id
    );
}

async fn setup_missing_rows(
    app: &axum::Router,
    tmp: &tempfile::TempDir,
) -> (String, String, String) {
    let root = tmp.path().join("movies-missing");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("Movie.A.1999.mkv"), b"video-a").unwrap();
    std::fs::write(root.join("Movie.B.2000.mkv"), b"video-b").unwrap();

    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "缺漏测试库",
                    "kind": "movie",
                    "root_paths": [root.display().to_string()],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let lib_id = created["data"]["id"].as_str().unwrap().to_string();

    scan_and_wait(app, &lib_id).await;

    let items = get(app, &format!("/api/v1/libraries/{lib_id}/items")).await;
    let id_a = items["data"][0]["media_item_id"]
        .as_str()
        .unwrap()
        .to_string();
    let id_b = items["data"][1]["media_item_id"]
        .as_str()
        .unwrap()
        .to_string();

    std::fs::remove_file(root.join("Movie.A.1999.mkv")).unwrap();
    std::fs::remove_file(root.join("Movie.B.2000.mkv")).unwrap();

    let verify = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{lib_id}/verify"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(verify.status(), StatusCode::OK);
    (lib_id, id_a, id_b)
}

#[tokio::test]
async fn delete_missing_rows_scoped_by_media_item_id() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (lib_id, id_a, id_b) = setup_missing_rows(&app, &tmp).await;

    // 仅删除 A 的缺失记录
    let del_a = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/libraries/{lib_id}/missing-rows?media_item_id={id_a}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(del_a.status(), StatusCode::OK);
    assert_eq!(json_body(del_a).await["data"]["deleted"], 1);

    // 查看 items：A 被删除，只剩 B
    let items_after = get(&app, &format!("/api/v1/libraries/{lib_id}/items")).await;
    let after_arr = items_after["data"].as_array().unwrap();
    assert_eq!(after_arr.len(), 1);
    assert_eq!(after_arr[0]["media_item_id"], id_b);

    // 全量删除清理 B
    let del_all = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/libraries/{lib_id}/missing-rows"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(del_all.status(), StatusCode::OK);
    assert_eq!(json_body(del_all).await["data"]["deleted"], 1);

    let items_final = get(&app, &format!("/api/v1/libraries/{lib_id}/items")).await;
    assert_eq!(items_final["data"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn created_library_keeps_intro_and_fingerprint_off_until_edited() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let root = tmp.path().join("kids");
    std::fs::create_dir_all(&root).unwrap();
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "儿童剧",
                    "kind": "tv",
                    "root_paths": [root.display().to_string()],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(created["data"]["detect_intros"], false);
    assert_eq!(created["data"]["enable_fingerprint"], false);

    let id = created["data"]["id"].as_str().unwrap();
    let patched = json_body(
        app.clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/v1/libraries/{id}"),
                Some("management-secret"),
                json!({ "detect_intros": true, "enable_fingerprint": true }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(patched["data"]["detect_intros"], true);
    assert_eq!(patched["data"]["enable_fingerprint"], true);
}

/// `/libraries` 的逐文件 `stat` / root `is_dir` / 封面 `is_file` 必须在 Store 锁
/// **外**、且在 http_work 有界预算内执行（P2-6）。
///
/// 证据形状：占住唯一 permit 后，列表请求必须**不再原地做 IO**，而是被预算拒绝
/// （503 runtime.busy）——说明文件观察已经搬到预算后面、离开了请求路径；放行预算
/// 后同一个请求返回与基线**逐字段一致**的 JSON。旧实现在锁内直接 stat，预算被占住
/// 也照常回 200，因此本测试对旧实现是红的。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn library_list_file_io_runs_under_the_bounded_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();

    let budget = Arc::new(api::blocking_budget::BlockingBudget::new(1));
    let st = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
    .with_http_work(budget.clone());
    let app = router(st);
    let _ = create_library(&app, "电影", "movie", &[root.display().to_string()]).await;

    // 基线：预算空闲时列表正常返回，root 存在、无文件。
    let baseline = get(&app, "/api/v1/libraries").await;
    let before = baseline["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|library| library["name"] == "电影")
        .expect("列表必须包含刚建的库")
        .clone();
    assert_eq!(before["root_missing"], false);
    assert_eq!(before["stats"]["file_count"], 0);

    // 占住唯一 permit：文件观察此刻无法进入 blocking 预算。
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = tokio::spawn({
        let budget = budget.clone();
        async move {
            budget
                .run_to_completion(move || {
                    let _ = entered_tx.send(());
                    let _ = release_rx.recv();
                })
                .await
        }
    });
    entered_rx.await.unwrap();

    let blocked = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/libraries",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(
        blocked.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "文件观察必须走 http_work 预算，不能在请求路径上原地执行"
    );
    assert_eq!(json_body(blocked).await["error"]["code"], "runtime.busy");

    // 预算被占住期间 Store 锁必须空闲：另一个要锁的请求照常返回。
    let other = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        app.clone().oneshot(request(
            "GET",
            "/api/v1/search/history",
            Some("management-secret"),
            Value::Null,
        )),
    )
    .await
    .expect("文件观察不得持有 Store 锁")
    .unwrap();
    assert_eq!(other.status(), StatusCode::OK);

    // 放行预算：同一请求恢复，字段与基线一致（含 root_missing 与 stats）。
    release_tx.send(()).unwrap();
    holder.await.unwrap().unwrap();
    let after = get(&app, "/api/v1/libraries").await;
    let after_library = after["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|library| library["name"] == "电影")
        .expect("列表必须包含刚建的库");
    assert_eq!(after_library, &before, "预算恢复后列表 JSON 必须与基线一致");
}
