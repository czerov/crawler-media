//! 搜索预算准入的 public-seam 行为：超预算站点降级、SSE 断开后不再排新站点。

#[path = "support/mod.rs"]
mod support;

use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use support::library_presence::PresenceFixture;
use support::search_admission::{ReleaseOnDrop, SearchAdmissionFixture};
use tower::ServiceExt;

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn search_and_fs_budgets_are_bounded() {
    let fix = PresenceFixture::new();

    assert_eq!(fix.state.search_work.available_permits(), 4);
    assert_eq!(fix.state.fs_work.available_permits(), 2);
    assert_eq!(fix.state.http_work.available_permits(), 8);
}

/// 4 个 permit 被占满时，第 5 个站点必须降级为单站失败，而不是挂起或让整请求 500。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn site_over_capacity_degrades_to_per_site_failure() {
    let fix = SearchAdmissionFixture::new(5);
    let _release = ReleaseOnDrop(fix.fetcher.clone());
    let occupying = fix.sites_query(&[0, 1, 2, 3]);
    let over_capacity = fix.sites_query(&[4]);

    let app = fix.app();
    let token = fix.admin_token.clone();
    let holder_uri = format!("/api/v1/search/torrents?keyword=matrix&sites={occupying}");
    let holder = tokio::spawn(async move {
        let request = Request::builder()
            .method("GET")
            .uri(holder_uri)
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        app.oneshot(request).await.unwrap()
    });
    assert!(
        fix.fetcher.wait_started(4, Duration::from_secs(5)).await,
        "占位请求应先占满 4 个搜索 permit"
    );

    let over_uri = format!("/api/v1/search/torrents?keyword=matrix&sites={over_capacity}");
    let response = tokio::time::timeout(Duration::from_secs(5), fix.send(&over_uri))
        .await
        .expect("超预算站点必须立即降级为单站失败，而不是挂起等待 permit");
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let sites = body["data"]["sites"].as_array().unwrap();
    assert_eq!(sites.len(), 1, "超预算站点仍应出现在 sites 报告里: {body}");
    let error = sites[0]["error"].as_str().unwrap_or_default();
    assert!(!error.is_empty(), "超预算站点必须报告失败原因: {body}");
    assert!(
        error.contains("并发"),
        "用户可见的失败原因必须是可读中文（web/lib/api/search.ts 契约）: {error}"
    );
    assert!(body["data"]["items"].as_array().unwrap().is_empty());

    fix.fetcher.release();
    let held = tokio::time::timeout(Duration::from_secs(5), holder)
        .await
        .expect("占位请求应在放行后返回")
        .unwrap();
    assert_eq!(held.status(), StatusCode::OK);
    let held_body = json_body(held).await;
    let held_sites = held_body["data"]["sites"].as_array().unwrap();
    assert_eq!(held_sites.len(), 4);
    assert!(
        held_sites.iter().all(|site| site["error"].is_null()),
        "获得 permit 的站点不应报错: {held_body}"
    );
}

/// 单个请求里站点数超过并发预算时，多出来的站点必须排队补位，而不是被本请求
/// 自己的预算判成过载饿死（P1：搜索 fanout 自饥饿）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sites_beyond_the_window_are_still_searched_in_one_request() {
    let fix = SearchAdmissionFixture::new(6);
    let _release = ReleaseOnDrop(fix.fetcher.clone());
    let app = fix.app();
    let token = fix.admin_token.clone();
    let handle = tokio::spawn(async move {
        let request = Request::builder()
            .method("GET")
            .uri("/api/v1/search/torrents?keyword=matrix")
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        app.oneshot(request).await.unwrap()
    });
    assert!(
        fix.fetcher.wait_started(4, Duration::from_secs(5)).await,
        "窗口内的 4 个站点应先开始"
    );
    // 放行：前 4 个完成后，剩下 2 个必须补位；旧实现此时已把它们判成过载。
    fix.fetcher.release();
    let response = tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("窗口补位不得让整请求挂起")
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let sites = body["data"]["sites"].as_array().unwrap();
    assert_eq!(
        sites.len(),
        6,
        "每个启用站点都必须出现在 sites 报告里: {body}"
    );
    let failed: Vec<&str> = sites
        .iter()
        .filter_map(|site| site["error"].as_str())
        .collect();
    assert!(
        failed.is_empty(),
        "同一请求内的站点不得互相饿死（旧实现只有前 4 个能搜）: {failed:?}"
    );
    assert_eq!(
        fix.fetcher.started().len(),
        6,
        "6 个站点都必须真的发起过抓取: {:?}",
        fix.fetcher.started()
    );
}

/// SSE 客户端断开后，历史条目与快照必须成对：历史里有条目就必须能回放，
/// 不能留下点开就 404 的死链（P2-1）。前端在换关键词/卸载时会正常 abort。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn aborted_stream_leaves_no_orphan_history_entry() {
    let fix = SearchAdmissionFixture::new(2);
    let _release = ReleaseOnDrop(fix.fetcher.clone());

    let response = fix
        .send("/api/v1/search/torrents/stream?keyword=matrix&save_history=true")
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        fix.fetcher.wait_started(2, Duration::from_secs(5)).await,
        "两个站点都应在窗口内开始"
    );
    // 客户端中断：响应 body 被丢弃 → rx 关闭。
    drop(response);
    fix.fetcher.release();

    // 服务端收尾后才会出现历史条目（快照先落盘、历史条目后写）。
    let mut history_id = None;
    for _ in 0..100 {
        let history = fix.send("/api/v1/search/history").await;
        let body = json_body(history).await;
        if let Some(id) = body["data"]["items"]
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item["id"].as_str())
        {
            history_id = Some(id.to_string());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let Some(id) = history_id else {
        panic!("被中断的流式搜索也应留下可回放的历史条目，而不是只留下关键词");
    };

    let snapshot = fix
        .send(&format!("/api/v1/search/history/{id}?vertical=torrents"))
        .await;
    assert_ne!(
        snapshot.status(),
        StatusCode::NOT_FOUND,
        "历史里有条目就必须能取到快照（不能是 404 死链）"
    );
    let body = json_body(snapshot).await;
    assert_eq!(
        body["data"]["partial"], true,
        "客户端中断时落盘的快照必须标记为半截结果: {body}"
    );
}

/// SSE 客户端断开后，服务端不再为剩余站点排队（B3 §6）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stream_stops_queuing_sites_after_client_disconnects() {
    let fix = SearchAdmissionFixture::new(8);
    let _release = ReleaseOnDrop(fix.fetcher.clone());

    let response = fix
        .send("/api/v1/search/torrents/stream?keyword=matrix")
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        fix.fetcher.wait_started(4, Duration::from_secs(5)).await,
        "窗口内的站点应开始搜索"
    );
    assert_eq!(
        fix.fetcher.started().len(),
        4,
        "开始阶段不应把全部 8 个站点都排上"
    );

    // 客户端断开：响应 body 被丢弃 → rx 关闭 → tx.is_closed() 为真。
    drop(response);
    fix.fetcher.release();
    assert!(
        fix.fetcher.wait_completed(4, Duration::from_secs(5)).await,
        "已开始的 4 个站点应能跑完"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        fix.fetcher.started().len(),
        4,
        "客户端断开后不得再排新站点: {:?}",
        fix.fetcher.started()
    );
}
