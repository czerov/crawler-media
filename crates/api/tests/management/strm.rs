//! STRM playback: scan a .strm file into the ledger and serve its remote URL
//! as a 302 redirect on /Videos/{id}/stream.

use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::body::to_bytes;
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
async fn strm_file_scans_and_stream_redirects_to_remote_url() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let movie = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&movie).unwrap();
    let remote = "https://cdn.example.com/media/The.Matrix.1999.2160p.mkv";
    std::fs::write(
        movie.join("The.Matrix.1999.2160p.strm"),
        format!("{remote}\n"),
    )
    .unwrap();

    let libs = json_body(
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
    let movie_id = libs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    scan_and_wait(&app, &movie_id).await;

    let ledger = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/ledger",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        ledger["data"].as_array().unwrap().len(),
        1,
        "strm file ledgered"
    );
    let compact = ledger["data"][0]["id"].as_str().unwrap().replace('-', "");

    let anonymous = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri(format!("/Videos/{compact}/stream"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    assert!(anonymous.headers().get("location").is_none());

    let stream = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri(format!("/Videos/{compact}/stream"))
                .header("authorization", "Bearer management-secret")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::FOUND);
    assert_eq!(
        stream
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok()),
        Some(remote)
    );
    let _ = to_bytes(stream.into_body(), 1024).await;
}
