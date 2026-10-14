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
        error.contains("capacity"),
        "失败原因应说明预算耗尽: {error}"
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
