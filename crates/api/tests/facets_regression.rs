//! B4：facets 十维契约与「排除自身维」计数回归。
//!
//! 基线 `facets` handler 没有 Query extractor、按 ledger 行数统计、`genres` 恒为
//! `[]`——筛选面板的类型/年代/地区/观看四维因此永远是空的。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use api::{ApiState, router};
use axum::http::StatusCode;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, UserRole};
use downloader::MemoryDownloader;
use parking_lot::Mutex as ParkingMutex;
use serde_json::Value;
use tower::ServiceExt;

#[path = "management/common.rs"]
mod common;
use common::{Fixtures, json_data, request, state};

fn base_state(tmp: &tempfile::TempDir) -> ApiState {
    state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: ParkingMutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
}

fn movie_library_id(state: &ApiState) -> String {
    state
        .store()
        .lock()
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|library| library.kind == MediaKind::Movie)
        .expect("default movie library")
        .id
}

fn library_root(state: &ApiState, library_id: &str) -> PathBuf {
    state
        .store()
        .lock()
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|library| library.id == library_id)
        .expect("library")
        .root_paths
        .first()
        .cloned()
        .expect("library root")
}

fn nfo_body(
    genres: &[&str],
    countries: &[&str],
    rating: &str,
    runtime: &str,
    language: &str,
) -> String {
    let genres: String = genres
        .iter()
        .map(|genre| format!("<genre>{genre}</genre>"))
        .collect();
    let countries: String = countries
        .iter()
        .map(|country| format!("<country>{country}</country>"))
        .collect();
    format!(
        "<movie><title>t</title><rating>{rating}</rating><runtime>{runtime}</runtime>\
         <original_language>{language}</original_language>{genres}{countries}</movie>"
    )
}

/// 落一个 Media + `files` 个 ledger 行（NFO 只写在第一个文件旁）。
fn seed_movie(
    state: &ApiState,
    root: &Path,
    title: &str,
    year: Option<u16>,
    resolution: Option<&str>,
    nfo: &str,
    files: usize,
) -> MediaId {
    let media_id = MediaId::new();
    state
        .store()
        .lock()
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Movie,
            title: title.into(),
            year,
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let dir = root.join(title);
    std::fs::create_dir_all(&dir).unwrap();
    for index in 0..files {
        let path = dir.join(format!("{title}-{index}.mkv"));
        std::fs::write(&path, b"video-bytes").unwrap();
        // 每个文件旁都有 NFO：`preferred_row` 可能挑中任意一行。
        std::fs::write(path.with_extension("nfo"), nfo).unwrap();
        let _ = index;
        state
            .store()
            .lock()
            .insert_ledger(&LedgerRow {
                id: LedgerId::new(),
                media_id,
                path: path.to_string_lossy().to_string(),
                season: None,
                episode: None,
                resolution: resolution.map(str::to_string),
                codec: Some("H264".into()),
                hdr: None,
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                filter_score: None,
            })
            .unwrap();
    }
    media_id
}

/// 三部作品：M1 动作/US/2024、M2 动作/JP/2015（两个文件）、M3 剧情/US/2024。
/// ledger 行数 = 4，distinct Media = 3。
fn seed_catalog(state: &ApiState, tmp: &tempfile::TempDir) -> (axum::Router, String) {
    let library_id = movie_library_id(state);
    let root = library_root(state, &library_id);
    seed_movie(
        state,
        &root,
        "Alpha",
        Some(2024),
        Some("1080p"),
        &nfo_body(
            &["Action"],
            &["United States of America"],
            "8.5",
            "100",
            "en",
        ),
        1,
    );
    seed_movie(
        state,
        &root,
        "Bravo",
        Some(2015),
        Some("720p"),
        &nfo_body(&["Action"], &["Japan"], "6.5", "70", "ja"),
        2,
    );
    seed_movie(
        state,
        &root,
        "Charlie",
        Some(2024),
        None,
        &nfo_body(
            &["Drama"],
            &["United States of America"],
            "7.5",
            "130",
            "en",
        ),
        1,
    );
    (router(state.clone()), library_id)
}

async fn facets(app: &axum::Router, url: &str) -> Value {
    let response = app
        .clone()
        .oneshot(request("GET", url, Some("management-secret"), Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{url}");
    json_data(response).await
}

fn find<'a>(values: &'a Value, dimension: &str, value: &str) -> Option<&'a Value> {
    values[dimension]
        .as_array()?
        .iter()
        .find(|entry| entry["value"] == value)
}

fn values_of(values: &Value, dimension: &str) -> Vec<String> {
    values[dimension]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["value"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn genres_are_counted_by_media_and_total_is_not_the_ledger_row_count() {
    let tmp = tempfile::tempdir().unwrap();
    let state = base_state(&tmp);
    let (app, library_id) = seed_catalog(&state, &tmp);

    // 真红：基线 `genres` 恒为 []。
    let filtered = facets(&app, &format!("/api/v1/libraries/{library_id}/facets?g=28")).await;
    let action = find(&filtered, "genres", "28").expect("genre 28 must be a candidate");
    assert_eq!(action["count"], 2, "genre=28 覆盖 Alpha 与 Bravo");
    assert_eq!(action["label"], "动作");
    assert_eq!(
        filtered["total"], 2,
        "total 是 distinct Media 数（不是 4 行 ledger）"
    );

    let unfiltered = facets(&app, &format!("/api/v1/libraries/{library_id}/facets")).await;
    assert_eq!(unfiltered["total"], 3, "3 部作品、4 个文件");
    assert_eq!(
        find(&unfiltered, "genres", "28").unwrap()["count"],
        2,
        "同一 Media 的多个文件只计一次"
    );
}

#[tokio::test]
async fn country_counts_exclude_their_own_dimension() {
    let tmp = tempfile::tempdir().unwrap();
    let state = base_state(&tmp);
    let (app, library_id) = seed_catalog(&state, &tmp);

    let values = facets(
        &app,
        &format!("/api/v1/libraries/{library_id}/facets?g=28&c=JP"),
    )
    .await;
    assert_eq!(values["total"], 1, "动作 + 日本 = Bravo");
    assert_eq!(
        find(&values, "countries", "US").unwrap()["count"],
        1,
        "US 的计数按「除 country 外」= genre 28 计算（Alpha）"
    );
    assert_eq!(find(&values, "countries", "JP").unwrap()["count"], 1);
}

#[tokio::test]
async fn tier_selects_the_filled_dimensions() {
    let tmp = tempfile::tempdir().unwrap();
    let state = base_state(&tmp);
    let (app, library_id) = seed_catalog(&state, &tmp);

    let primary = facets(
        &app,
        &format!("/api/v1/libraries/{library_id}/facets?tier=primary"),
    )
    .await;
    for dimension in ["genres", "countries", "decades", "watch"] {
        assert!(
            !primary[dimension].as_array().unwrap().is_empty(),
            "{dimension} must be filled for tier=primary"
        );
    }
    for dimension in [
        "ratings",
        "runtimes",
        "languages",
        "resolutions",
        "hdr",
        "stock",
    ] {
        assert!(
            primary[dimension].as_array().unwrap().is_empty(),
            "{dimension} must stay empty for tier=primary"
        );
    }

    let all = facets(
        &app,
        &format!("/api/v1/libraries/{library_id}/facets?tier=all"),
    )
    .await;
    assert!(!values_of(&all, "resolutions").is_empty());
    assert!(!values_of(&all, "languages").is_empty());
    assert!(!values_of(&all, "ratings").is_empty());

    // 缺省 tier 与 all 等价（旧调用方仍能拿到 resolutions）。
    let default_tier = facets(&app, &format!("/api/v1/libraries/{library_id}/facets")).await;
    assert_eq!(
        values_of(&default_tier, "resolutions"),
        values_of(&all, "resolutions")
    );
    assert!(default_tier["years"].as_array().unwrap().is_empty());

    let response = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{library_id}/facets?tier=bogus"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn unknown_year_does_not_produce_a_2020s_candidate() {
    let tmp = tempfile::tempdir().unwrap();
    let state = base_state(&tmp);
    let library_id = movie_library_id(&state);
    let root = library_root(&state, &library_id);
    seed_movie(
        &state,
        &root,
        "NoYear",
        None,
        Some("1080p"),
        &nfo_body(
            &["Action"],
            &["United States of America"],
            "8.0",
            "90",
            "en",
        ),
        1,
    );
    seed_movie(
        &state,
        &root,
        "Nineties",
        Some(1995),
        Some("1080p"),
        &nfo_body(&["Drama"], &["Japan"], "7.0", "95", "ja"),
        1,
    );

    let values = facets(
        &router(state.clone()),
        &format!("/api/v1/libraries/{library_id}/facets"),
    )
    .await;
    let decades = values_of(&values, "decades");
    assert_eq!(decades, vec!["1990s".to_string()], "{values}");
    assert!(
        !decades.contains(&"2020s".to_string()),
        "未知年份不得伪归 2020s：{values}"
    );
}

#[tokio::test]
async fn hidden_library_does_not_contribute_candidates() {
    let tmp = tempfile::tempdir().unwrap();
    let state = base_state(&tmp);
    let (app, library_id) = seed_catalog(&state, &tmp);

    // 一个成员看不见的库（selected 且成员名单为空），里面只有 Horror。
    let hidden_root = tmp.path().join("hidden");
    std::fs::create_dir_all(&hidden_root).unwrap();
    let hidden = state
        .store()
        .lock()
        .create_library(
            MediaKind::Movie,
            "Hidden",
            &[hidden_root.to_str().unwrap()],
            "selected",
            true,
            &[],
        )
        .unwrap();
    seed_movie(
        &state,
        &hidden_root,
        "Hidden Horror",
        Some(2024),
        Some("1080p"),
        &nfo_body(&["Horror"], &["Japan"], "6.0", "80", "ja"),
        1,
    );

    let member_token = {
        let store = state.store();
        let guard = store.lock();
        let user = domain::User {
            id: domain::UserId::new(),
            login: "facet-member".into(),
            enabled: true,
            role: UserRole::Member,
        };
        guard
            .insert_user_with_password(&user, "member-pass")
            .unwrap();
        let token = "facet-member-session".to_string();
        guard.set_user_token(user.id, &token).unwrap();
        token
    };

    let response = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{}/facets", hidden.id),
            Some(&member_token),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let visible = {
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{library_id}/facets"),
                Some(&member_token),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        json_data(response).await
    };
    assert!(
        find(&visible, "genres", "27").is_none(),
        "隐藏库的条目不得进入候选：{visible}"
    );
}
