//! Chapters + relax-filter endpoints on seeded libraries.

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

async fn seed(tmp: &tempfile::TempDir, app: &axum::Router) -> (String, String) {
    let movie = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&movie).unwrap();
    std::fs::write(movie.join("The.Matrix.1999.2160p.mkv"), b"m").unwrap();
    std::fs::write(movie.join("Dune.2021.1080p.mkv"), b"d").unwrap();
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
    (
        movie_id,
        ledger["data"][0]["media_id"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn chapters_endpoints_respond_without_embedded_chapters() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (movie_id, media_id) = seed(&tmp, &app).await;
    let chapters = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{movie_id}/items/{media_id}/chapters"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        chapters["data"].as_array().unwrap().is_empty(),
        "fake file has no chapters"
    );
    let generate = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{movie_id}/items/{media_id}/chapters/generate"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(
        generate.status(),
        StatusCode::BAD_REQUEST,
        "no chapters → 400"
    );
    let error_body = json_body(generate).await;
    assert_eq!(
        error_body["error"]["code"],
        "chapters.none",
        "error code should be chapters.none"
    );
}

#[tokio::test]
async fn chapters_generate_with_voiceprint_markers() {
    use std::str::FromStr;
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let api_state = state(tmp.path(), fetcher, downloader);
    let app = router(api_state.clone());
    let (movie_id, media_id) = seed(&tmp, &app).await;

    // 预置片头片尾标记
    let m_id = domain::MediaId::from_str(&media_id).unwrap();
    {
        let store_arc = api_state.store();
        let store = store_arc.lock();
        store.put_media_marker(&api::store::StoredMediaMarker {
            media_id: m_id,
            season: 1,
            episode: 1,
            intro_start_ms: Some(10_000),
            intro_end_ms: Some(90_000),
            outro_start_ms: Some(1_200_000),
            outro_end_ms: Some(1_300_000),
            source: "voiceprint".into(),
            locked: false,
            updated_at: 1,
        }).unwrap();
    }

    let generate = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{movie_id}/items/{media_id}/chapters/generate"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();

    // 伪造文件无真实视频流，ffmpeg 提取帧会失败，但能证明不再报 chapters.none 400
    let res = json_body(generate).await;
    assert_eq!(res["ok"], true);
    // marker::build_complete_timeline_chapters 拆分成4段（序幕、片头、正片、片尾）
    assert_eq!(res["data"]["total"], 4);
}

#[tokio::test]
async fn wall_filter_and_relax_use_resolutions() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (movie_id, _media_id) = seed(&tmp, &app).await;

    let all = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{movie_id}/items"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(all["data"].as_array().unwrap().len(), 2);

    let filtered = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{movie_id}/items?resolutions=2160p"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        filtered["data"].as_array().unwrap().len(),
        1,
        "resolution filter applies"
    );
    assert_eq!(filtered["data"][0]["title"], "The Matrix");

    let relax = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/libraries/{movie_id}/items/relax-filter"),
                Some("management-secret"),
                json!({ "resolutions": ["2160p", "1080p"] }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let suggestions = relax["data"]["suggestions"].as_array().unwrap();
    assert_eq!(suggestions.len(), 2);
    assert_eq!(suggestions[0]["dim"], "resolutions");
    assert!(suggestions[0]["count"].as_i64().unwrap() >= 1);
}
