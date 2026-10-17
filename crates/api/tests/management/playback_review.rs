//! Regression coverage for exact Playback units and default resume.
use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

pub(super) fn fixture(tmp: &tempfile::TempDir, tv: bool) -> (axum::Router, Value) {
    let state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    );
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: if tv {
            domain::MediaKind::Tv
        } else {
            domain::MediaKind::Movie
        },
        title: "Playback fixture".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        bangumi_id: None,
        anilist_id: None,
        tvdb_id: None,
    };
    let row = domain::LedgerRow {
        id: domain::LedgerId::new(),
        media_id: media.id,
        path: tmp
            .path()
            .join(if tv {
                "data/library/tv/episode.mkv"
            } else {
                "data/library/movies/movie.mkv"
            })
            .to_string_lossy()
            .into(),
        season: tv.then_some(2),
        episode: tv.then_some(3),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: domain::QualitySource::Probe,
        confidence: domain::Confidence::High,
        filter_score: None,
    };
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    store.insert_media(&media).unwrap();
    store.insert_ledger(&row).unwrap();
    (
        router(state),
        json!({ "media_item_id": media.id.to_string(), "file_id": row.id.to_string() }),
    )
}

pub(super) async fn post(app: &axum::Router, route: &str, body: Value) -> axum::response::Response {
    app.clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/playback/{route}"),
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap()
}

pub(super) async fn save_position(app: &axum::Router, target: &Value) {
    let mut body = target.clone();
    body["device_id"] = json!("resume-device");
    body["position_ms"] = json!(600_000);
    assert_eq!(post(app, "progress", body).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn absent_start_resumes_saved_movie_position() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, target) = fixture(&tmp, false);
    save_position(&app, &target).await;
    let session = json_data(post(&app, "sessions", target).await).await;
    assert_eq!(session["start_ms"], 600_000);
}

#[tokio::test]
async fn explicit_zero_overrides_saved_position() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, mut target) = fixture(&tmp, false);
    save_position(&app, &target).await;
    target["start_ms"] = json!(0);
    let session = json_data(post(&app, "sessions", target).await).await;
    assert_eq!(session["start_ms"], 0);
}

#[tokio::test]
async fn completed_movie_replays_from_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, target) = fixture(&tmp, false);
    save_position(&app, &target).await;
    let mut mark = target.clone();
    mark["played"] = json!(true);
    assert_eq!(post(&app, "marks", mark).await.status(), StatusCode::OK);
    let session = json_data(post(&app, "sessions", target).await).await;
    assert_eq!(session["start_ms"], 0);
}

#[tokio::test]
async fn omitted_tv_coordinates_infer_selected_file_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, mut target) = fixture(&tmp, true);
    let mut explicit = target.clone();
    explicit["season_number"] = json!(2);
    explicit["episode_number"] = json!(3);
    save_position(&app, &explicit).await;
    let session = json_data(post(&app, "sessions", target.clone()).await).await;
    assert_eq!(session["watch"]["position_ms"], 600_000);
    assert_eq!(session["start_ms"], 600_000);
    target["device_id"] = json!("inferred-device");
    target["position_ms"] = json!(700_000);
    assert_eq!(
        post(&app, "progress", target.clone()).await.status(),
        StatusCode::OK
    );
    let session = json_data(post(&app, "sessions", explicit).await).await;
    assert_eq!(session["watch"]["position_ms"], 700_000);
}

#[tokio::test]
async fn missing_exact_episode_never_falls_back_to_another_file() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, mut target) = fixture(&tmp, true);
    target.as_object_mut().unwrap().remove("file_id");
    target["season_number"] = json!(2);
    target["episode_number"] = json!(99);
    target["device_id"] = json!("missing-device");
    for route in ["decide", "sessions", "progress"] {
        assert_eq!(
            post(&app, route, target.clone()).await.status(),
            StatusCode::NOT_FOUND,
            "{route}"
        );
    }
}

#[tokio::test]
async fn file_id_with_inconsistent_unit_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, target) = fixture(&tmp, true);
    for (season, episode) in [(1, 3), (2, 99)] {
        let mut body = target.clone();
        body["season_number"] = json!(season);
        body["episode_number"] = json!(episode);
        body["device_id"] = json!("mismatch-device");
        for route in ["decide", "sessions", "progress"] {
            assert_eq!(
                post(&app, route, body.clone()).await.status(),
                StatusCode::NOT_FOUND,
                "{route}"
            );
        }
    }
}

#[tokio::test]
async fn invalid_start_is_rejected_instead_of_silently_starting_at_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, target) = fixture(&tmp, false);
    for start in [
        json!(-1),
        json!("bad"),
        json!(1.5),
        Value::Null,
        json!(true),
        json!(u64::MAX),
    ] {
        let mut body = target.clone();
        body["start_ms"] = start;
        assert_eq!(
            post(&app, "sessions", body).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn partial_coordinates_select_only_matching_season() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, mut target) = fixture(&tmp, true);
    target.as_object_mut().unwrap().remove("file_id");
    target["season"] = json!(99);
    assert_eq!(
        post(&app, "sessions", target).await.status(),
        StatusCode::NOT_FOUND
    );
}
