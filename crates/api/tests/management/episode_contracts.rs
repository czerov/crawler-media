//! Contract tests for scoped episode lists and per-user watch state.

use std::collections::HashMap;
use std::sync::Arc;

use api::ApiState;
use api::router;
use axum::http::StatusCode;
use domain::{LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, UserId};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::{Fixtures, json_data, json_error, request, state};

async fn create_user_and_login(app: &axum::Router, name: &str, pass: &str) -> (UserId, String) {
    let user_resp = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/users",
            Some("management-secret"),
            json!({ "login": name, "password": pass }),
        ))
        .await
        .unwrap();
    assert_eq!(user_resp.status(), StatusCode::CREATED);
    let uid: UserId = json_data(user_resp).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let login = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/auth/login",
            None,
            json!({ "username": name, "password": pass }),
        ))
        .await
        .unwrap();
    let token = json_data(login).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    (uid, token)
}

fn create_sample_media(media_id: MediaId) -> Media {
    Media {
        id: media_id,
        kind: MediaKind::Tv,
        title: "Test Show".into(),
        year: Some(2024),
        original_title: Some("Test Show".into()),
        aliases: Vec::new(),
        tmdb_id: Some("12345".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn seed_library_files(dir: &std::path::Path, media_id: MediaId) -> Vec<LedgerRow> {
    std::fs::create_dir_all(dir).unwrap();
    let files = [
        (1, 1, "Test Show S01E01.mkv"),
        (1, 2, "Test Show S01E02.mkv"),
        (2, 1, "Test Show S02E01.mkv"),
    ];
    let mut rows = Vec::new();
    for (s, ep, name) in files {
        let p = dir.join(name);
        std::fs::write(&p, b"video").unwrap();
        rows.push(LedgerRow {
            id: LedgerId::new(),
            media_id,
            path: p.to_string_lossy().to_string(),
            season: Some(s),
            episode: Some(ep),
            resolution: Some("1080p".into()),
            codec: Some("H264".into()),
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: domain::Confidence::High,
            filter_score: None,
        });
    }
    rows
}

fn seed_test_env(
    api_state: &ApiState,
    user_a_id: UserId,
    media_id: MediaId,
    tmp_path: &std::path::Path,
) {
    let media = create_sample_media(media_id);
    let library_dir = tmp_path.join("library").join("tv").join("Test Show");
    let rows = seed_library_files(&library_dir, media_id);

    let store = api_state.store();
    let s = store.lock();
    s.insert_media(&media).unwrap();
    for row in &rows {
        s.insert_ledger(row).unwrap();
    }
    s.upsert_unit(
        user_a_id,
        media_id,
        1,
        1,
        300_000,
        Some(false),
        None,
        Some(1_200_000),
        None,
        None,
        false,
        1000,
    )
    .unwrap();
}

#[tokio::test]
async fn episodes_filtered_by_season_and_per_user_progress_isolated() {
    let tmp = tempfile::tempdir().unwrap();
    let api_state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    );
    let app = router(api_state.clone());

    let (user_a_id, token_a) = create_user_and_login(&app, "user-a", "password-a").await;
    let (_user_b_id, token_b) = create_user_and_login(&app, "user-b", "password-b").await;

    let media_id = MediaId::new();
    seed_test_env(&api_state, user_a_id, media_id, tmp.path());

    // Query S1 as User A
    let resp_a_s1 = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/playback/items/{media_id}/episodes?season_number=1"),
            Some(&token_a),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(resp_a_s1.status(), StatusCode::OK);
    let data_a_s1 = json_data(resp_a_s1).await;
    assert_eq!(data_a_s1["season_number"], 1);
    let episodes_a = data_a_s1["episodes"].as_array().unwrap();
    assert_eq!(episodes_a.len(), 2, "Should only have S1 episodes");
    assert_eq!(episodes_a[0]["episode_number"], 1);
    assert_eq!(episodes_a[0]["season_number"], 1);
    assert_eq!(episodes_a[0]["position_ms"], 300_000);
    assert_eq!(episodes_a[0]["played"], false);
    assert_eq!(episodes_a[0]["progress_percent"], 25);
    assert_eq!(episodes_a[1]["episode_number"], 2);
    assert_eq!(episodes_a[1]["season_number"], 1);
    assert_eq!(episodes_a[1]["position_ms"], 0);
    assert_eq!(episodes_a[1]["played"], false);
    assert_eq!(episodes_a[1]["progress_percent"], Value::Null);

    // Query S1 as User B (isolated)
    let resp_b_s1 = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/playback/items/{media_id}/episodes?season_number=1"),
            Some(&token_b),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(resp_b_s1.status(), StatusCode::OK);
    let data_b_s1 = json_data(resp_b_s1).await;
    assert_eq!(data_b_s1["season_number"], 1);
    let episodes_b = data_b_s1["episodes"].as_array().unwrap();
    assert_eq!(episodes_b.len(), 2);
    assert_eq!(episodes_b[0]["episode_number"], 1);
    assert_eq!(episodes_b[0]["season_number"], 1);
    assert_eq!(episodes_b[0]["played"], false);
    assert_eq!(episodes_b[0]["position_ms"], 0);
    assert_eq!(episodes_b[0]["progress_percent"], Value::Null);

    // Query S2 as User A
    let resp_s2 = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/playback/items/{media_id}/episodes?season_number=2"),
            Some(&token_a),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(resp_s2.status(), StatusCode::OK);
    let data_s2 = json_data(resp_s2).await;
    assert_eq!(data_s2["season_number"], 2);
    let episodes_s2 = data_s2["episodes"].as_array().unwrap();
    assert_eq!(episodes_s2.len(), 1, "Should only have S2 episode");
    assert_eq!(episodes_s2[0]["episode_number"], 1);
    assert_eq!(episodes_s2[0]["season_number"], 2);
    assert_eq!(episodes_s2[0]["position_ms"], 0);
    assert_eq!(episodes_s2[0]["played"], false);
    assert_eq!(episodes_s2[0]["progress_percent"], Value::Null);
}

#[tokio::test]
async fn library_episodes_reject_invalid_season_parameters() {
    let tmp = tempfile::tempdir().unwrap();
    let api_state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    );
    let app = router(api_state.clone());
    let (user_id, token) = create_user_and_login(&app, "season-user", "password").await;
    let media_id = MediaId::new();
    seed_test_env(&api_state, user_id, media_id, tmp.path());
    let library_id = api_state
        .store()
        .lock()
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|library| library.kind == MediaKind::Tv)
        .unwrap()
        .id;

    for season in ["abc", "-1", "4294967296", ""] {
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!(
                    "/api/v1/libraries/{library_id}/items/{media_id}/episodes?season={season}"
                ),
                Some(&token),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "season={season}"
        );
        let error = json_error(response).await;
        assert_eq!(error["code"], "library.invalid");
        assert_eq!(error["message"], "季号参数无效");
    }
}

#[tokio::test]
async fn library_episodes_accept_season_zero_for_specials() {
    let tmp = tempfile::tempdir().unwrap();
    let api_state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    );
    let app = router(api_state.clone());
    let (user_id, token) = create_user_and_login(&app, "specials-user", "password").await;
    let media_id = MediaId::new();
    seed_test_env(&api_state, user_id, media_id, tmp.path());
    let library_id = {
        let store = api_state.store();
        let store = store.lock();
        let mut special = store
            .list_ledger()
            .unwrap()
            .into_iter()
            .find(|row| row.media_id == media_id && row.season == Some(2))
            .unwrap();
        special.season = Some(0);
        store.insert_ledger(&special).unwrap();
        store
            .list_libraries()
            .unwrap()
            .into_iter()
            .find(|library| library.kind == MediaKind::Tv)
            .unwrap()
            .id
    };

    let response = app
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{library_id}/items/{media_id}/episodes?season=0"),
            Some(&token),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let episodes = json_data(response).await;
    let episodes = episodes.as_array().unwrap();
    assert_eq!(episodes.len(), 1, "Only specials belong to season zero");
    assert_eq!(episodes[0]["season_number"], 0);
    assert_eq!(episodes[0]["episode_number"], 1);
    assert_eq!(episodes[0]["media_item_id"], media_id.to_string());
    assert_eq!(episodes[0]["owned"], true);
    assert_eq!(episodes[0]["file_ids"].as_array().unwrap().len(), 1);
    assert_eq!(episodes[0]["position_ms"], 0);
    assert_eq!(episodes[0]["played"], false);
    assert_eq!(episodes[0]["progress_percent"], Value::Null);
}
