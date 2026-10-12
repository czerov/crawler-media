//! `/api/v1/fs/browse` 的表征回归。
//!
//! 目录枚举整体移入 blocking 线程后，对外契约必须不变：只列目录、跳过隐藏项、
//! 按名称大小写不敏感排序，缺失路径 404、非目录 400。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

#[path = "management/common.rs"]
mod common;
use common::{Fixtures, json_body, request, state};

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

async fn browse(app: &axum::Router, path: &Path) -> (StatusCode, Value) {
    let uri = format!("/api/v1/fs/browse?path={}", path.display());
    let response = app
        .clone()
        .oneshot(request("GET", &uri, Some("management-secret"), Value::Null))
        .await
        .unwrap();
    let status = response.status();
    (status, json_body(response).await)
}

fn names(body: &Value) -> Vec<String> {
    body["data"]["entries"]
        .as_array()
        .expect("entries array")
        .iter()
        .map(|entry| entry["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn lists_only_directories_sorted_case_insensitively() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    for dir in ["Zeta", "alpha", "Beta", ".hidden"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(root.join("notes.txt"), b"x").unwrap();

    let (status, body) = browse(&app(&tmp), &root).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        names(&body),
        vec!["alpha", "Beta", "Zeta"],
        "只列目录、跳过隐藏项、大小写不敏感排序"
    );
    // 返回的是 canonical 后的路径。
    assert!(body["data"]["path"].as_str().unwrap().ends_with("root"));
    assert!(body["data"]["parent"].is_string());
}

#[tokio::test]
async fn missing_directory_is_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("does-not-exist");
    let (status, body) = browse(&app(&tmp), &missing).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "fs.not_found");
}

#[tokio::test]
async fn file_path_is_bad_request() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.txt");
    std::fs::write(&file, b"x").unwrap();
    let (status, body) = browse(&app(&tmp), &file).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "fs.not_directory");
}
