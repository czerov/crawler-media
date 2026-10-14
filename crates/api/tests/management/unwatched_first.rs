//! 「未看优先」在两个行上的口径：`/libraries/{id}/items` 与 `/playback/favorites`。
//!
//! 规则是**排序**不是筛选（见 `library::latest` 的 `WatchTier`）：没开始看过的排最前、
//! 在看其次、已看完沉底，段内仍是这一档自己的排序。筛完只剩一张卡的小库看起来像坏了，
//! 而「最近添加」这个名字本来就该有内容。
//!
//! 墙上用户手选的「未观看」是另一回事：它走 `w`，永远严格筛，不会被这个开关放宽。

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

/// 建一个三部电影的电影库并扫描。返回 (library id, 按标题排好的三个 media id)——
/// 用例只按名次取用，不依赖文件名解析出来的标题长什么样。
async fn seed(tmp: &tempfile::TempDir, app: &axum::Router) -> (String, Vec<String>) {
    let movies = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&movies).unwrap();
    for (index, name) in ["Arrival.2016.2160p.mkv", "Dune.2021.2160p.mkv", "Matrix.1999.2160p.mkv"]
        .iter()
        .enumerate()
    {
        std::fs::write(movies.join(name), format!("m{index}")).unwrap();
    }
    let libraries = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/libraries",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let library_id = libraries
        .as_array()
        .unwrap()
        .iter()
        .find(|library| library["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    scan_and_wait(app, &library_id).await;
    let ids: Vec<String> = items(app, &library_id, "sort=title").await
        .iter()
        .map(|item| item["media_item_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 3, "扫描后应有三部电影");
    (library_id, ids)
}

/// 取一个库的条目；`query` 是完整查询串（`sort=...&unwatched_first=true` 之类）。
async fn items(app: &axum::Router, library_id: &str, query: &str) -> Vec<Value> {
    let body = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{library_id}/items?limit=50&{query}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    body.as_array().cloned().unwrap_or_default()
}

fn ids_of(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .map(|item| item["media_item_id"].as_str().unwrap().to_string())
        .collect()
}

async fn post(app: &axum::Router, uri: &str, body: Value) -> axum::response::Response {
    app.clone()
        .oneshot(request("POST", uri, Some("management-secret"), body))
        .await
        .unwrap()
}

/// 标成"已看完"（作品级标记）。
async fn mark_played(app: &axum::Router, media_item_id: &str) {
    let response = post(
        app,
        "/api/v1/playback/marks",
        json!({ "media_item_id": media_item_id, "played": true }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

/// 标成"在看"：有进度、没看完。
async fn mark_watching(app: &axum::Router, media_item_id: &str) {
    let response = post(
        app,
        "/api/v1/playback/progress",
        json!({
            "media_item_id": media_item_id,
            "event": "progress",
            "device_id": "web-1",
            "position_ms": 120_000,
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

async fn mark_favorite(app: &axum::Router, media_item_id: &str) {
    let response = post(
        app,
        "/api/v1/playback/marks",
        json!({ "media_item_id": media_item_id, "favorite": true }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn library_row_orders_unwatched_then_watching_then_finished() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (library_id, ids) = seed(&tmp, &app).await;
    mark_watching(&app, &ids[1]).await;
    mark_played(&app, &ids[2]).await;

    // 未看优先：三部都在，只是顺序变了——不是筛掉两部只剩一部
    let ordered = ids_of(&items(&app, &library_id, "sort=added_at&unwatched_first=true").await);
    assert_eq!(ordered, ids, "未看 → 在看 → 已看完");

    // 不带开关：同一批条目，顺序回到纯入库时间
    let plain = ids_of(&items(&app, &library_id, "sort=added_at").await);
    assert_eq!(plain.len(), 3);
    let mut sorted_plain = plain.clone();
    sorted_plain.sort();
    let mut sorted_ids = ids.clone();
    sorted_ids.sort();
    assert_eq!(sorted_plain, sorted_ids);
}

#[tokio::test]
async fn library_row_never_hides_a_fully_watched_library() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (library_id, ids) = seed(&tmp, &app).await;
    for id in &ids {
        mark_played(&app, id).await;
    }

    // 全看完了也照样有三部（只是都在"已看完"那一档）——这正是这个开关存在的理由
    let with_flag = ids_of(&items(&app, &library_id, "sort=added_at&unwatched_first=true").await);
    assert_eq!(with_flag.len(), 3);
    let mut sorted = with_flag;
    sorted.sort();
    let mut expected = ids.clone();
    expected.sort();
    assert_eq!(sorted, expected);
}

#[tokio::test]
async fn explicit_watch_filter_stays_strict_next_to_the_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (library_id, ids) = seed(&tmp, &app).await;
    mark_watching(&app, &ids[1]).await;
    mark_played(&app, &ids[2]).await;

    // 墙上手选的「未观看」：只有没开始看过的那一部，不会被未看优先放宽
    let strict = ids_of(&items(&app, &library_id, "sort=added_at&w=unwatched").await);
    assert_eq!(strict, vec![ids[0].clone()]);
    let both = ids_of(
        &items(
            &app,
            &library_id,
            "sort=added_at&w=unwatched&unwatched_first=true",
        )
        .await,
    );
    assert_eq!(both, vec![ids[0].clone()]);
}

#[tokio::test]
async fn unwatched_first_rejects_a_non_boolean() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (library_id, _) = seed(&tmp, &app).await;
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{library_id}/items?unwatched_first=maybe"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// 「我的收藏」行的同一档：一直是个没实现的参数（web 早在发，后端没人读），
/// 现在补上——同一个开关、同一条规则。
#[tokio::test]
async fn favorites_row_orders_unwatched_then_watching_then_finished() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (_, ids) = seed(&tmp, &app).await;
    for id in &ids {
        mark_favorite(&app, id).await;
    }
    mark_watching(&app, &ids[1]).await;
    mark_played(&app, &ids[2]).await;

    let ordered = favorites(&app, "unwatched_first=true").await;
    assert_eq!(ids_of(&ordered), ids, "未看 → 在看 → 已看完");

    // 不带开关：同一批收藏，只是没有分级（顺序交给收藏时间）
    let plain = favorites(&app, "").await;
    let mut sorted = ids_of(&plain);
    sorted.sort();
    let mut expected = ids.clone();
    expected.sort();
    assert_eq!(sorted, expected, "未看优先只改顺序，不改名单");
}

/// `GET /playback/favorites` 的 data.items。
async fn favorites(app: &axum::Router, extra: &str) -> Vec<Value> {
    let body = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/playback/favorites?limit=50&{extra}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    body["items"].as_array().cloned().unwrap_or_default()
}
