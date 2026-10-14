use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
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

#[tokio::test]
async fn item_index_and_facets_return_structured_dtos() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let movie_dir = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&movie_dir).unwrap();
    std::fs::write(movie_dir.join("Alien.1979.2160p.mkv"), b"1").unwrap();
    std::fs::write(movie_dir.join("Avatar.2009.1080p.mkv"), b"2").unwrap();
    std::fs::write(movie_dir.join("Blade.1998.1080p.mkv"), b"3").unwrap();

    let libs = json_data(
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
    let movie_id = libs
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // 扫描
    scan_and_wait(&app, &movie_id).await;

    // 1. 测试 item-index 桶聚合
    let index_res = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{movie_id}/item-index?sort=title&order=asc"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let buckets = index_res.as_array().unwrap();
    assert!(!buckets.is_empty(), "item-index 应返回非空首字母桶");
    // "Alien" 与 "Avatar" 首字母均为 A，count 应为 2，offset 0
    let a_bucket = buckets.iter().find(|b| b["initial"] == "A").unwrap();
    assert_eq!(a_bucket["count"], 2);
    assert_eq!(a_bucket["offset"], 0);
    // "Blade" 首字母 B，count 1，offset 2
    let b_bucket = buckets.iter().find(|b| b["initial"] == "B").unwrap();
    assert_eq!(b_bucket["count"], 1);
    assert_eq!(b_bucket["offset"], 2);

    // 2. 测试 facets 结构化 DTO
    let facets_res = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{movie_id}/facets"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(facets_res["total"], 3);
    let resolutions = facets_res["resolutions"].as_array().unwrap();
    assert!(!resolutions.is_empty());
    // 每个分辨率应该是一个对象 { value, label, count } 而不是裸字符串
    for res_item in resolutions {
        assert!(res_item.get("value").is_some());
        assert!(res_item.get("label").is_some());
        assert!(res_item.get("count").is_some());
    }
}
