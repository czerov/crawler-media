//! Tests for library gallery pagination, filters, and group contracts.

use std::collections::HashMap;
use std::sync::Arc;

use api::ApiState;
use api::router;
use axum::http::StatusCode;
use domain::{LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::{Fixtures, json_data, request, state};

fn seed_movie_with_artwork(api_state: &ApiState, lib_dir: &std::path::Path, i: usize) {
    let media_id = MediaId::new();
    let media = Media {
        id: media_id,
        kind: MediaKind::Movie,
        title: format!("Movie {i}"),
        year: Some(2020 + i as u16),
        original_title: Some(format!("Movie {i}")),
        aliases: Vec::new(),
        tmdb_id: Some(format!("100{i}")),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    api_state.store().lock().insert_media(&media).unwrap();

    let item_dir = lib_dir.join(format!("Movie {i}"));
    std::fs::create_dir_all(&item_dir).unwrap();
    let video_file = item_dir.join(format!("Movie {i}.mkv"));
    // Movie 3 is deliberately the largest so the `size` sort has something to order.
    let payload: &[u8] = if i == 3 { &[b'x'; 4096] } else { b"content" };
    std::fs::write(&video_file, payload).unwrap();
    let poster_file = item_dir.join("poster.jpg");
    std::fs::write(&poster_file, b"poster").unwrap();

    let row = LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: video_file.to_string_lossy().to_string(),
        season: None,
        episode: None,
        resolution: Some("1080p".into()),
        codec: Some("H264".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: domain::Confidence::High,
        filter_score: None,
    };
    api_state.store().lock().insert_ledger(&row).unwrap();
    if i == 1 {
        std::fs::write(
            video_file.with_extension("nfo"),
            "<movie><rating>8.5</rating><runtime>100</runtime><original_language>en</original_language><premiered>2021-06-01</premiered></movie>",
        )
        .unwrap();
    }
}

fn gallery_app(tmp: &tempfile::TempDir) -> (ApiState, axum::Router) {
    let api_state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    );
    let app = router(api_state.clone());
    (api_state, app)
}

async fn seed_gallery(tmp: &tempfile::TempDir, api_state: &ApiState, app: &axum::Router) -> String {
    let libs = fetch_gallery_json(app, "/api/v1/libraries?kind=movie").await;
    let lib_id = libs.as_array().unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let lib_dir = tmp.path().join("library").join("movies");
    for i in 1..=3 {
        seed_movie_with_artwork(api_state, &lib_dir, i);
    }
    lib_id
}

async fn fetch_gallery_json(app: &axum::Router, url: &str) -> Value {
    let resp = app
        .clone()
        .oneshot(request("GET", url, Some("management-secret"), Value::Null))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    json_data(resp).await
}

#[tokio::test]
async fn library_gallery_supports_stable_pagination_and_complete_dto() {
    let tmp = tempfile::tempdir().unwrap();
    let (api_state, app) = gallery_app(&tmp);
    let lib_id = seed_gallery(&tmp, &api_state, &app).await;

    let page1 = fetch_gallery_json(
        &app,
        &format!("/api/v1/libraries/{lib_id}/gallery?limit=2&offset=0"),
    )
    .await;
    let groups1 = page1.as_array().unwrap();
    assert_eq!(groups1.len(), 2, "Page 1 must have 2 groups");
    let g0 = &groups1[0];
    assert_eq!(g0["library_id"], lib_id);
    assert!(g0.get("media_item_id").is_some());
    assert!(g0.get("title").is_some());
    assert!(g0.get("is_favorite").is_some());
    let images = g0["images"].as_array().unwrap();
    assert!(!images.is_empty());
    assert_eq!(images[0]["library_id"], lib_id);

    let page2 = fetch_gallery_json(
        &app,
        &format!("/api/v1/libraries/{lib_id}/gallery?limit=2&offset=2"),
    )
    .await;
    let groups2 = page2.as_array().unwrap();
    assert_eq!(groups2.len(), 1, "Page 2 must have 1 group");
    let id1: Vec<&str> = groups1
        .iter()
        .map(|g| g["media_item_id"].as_str().unwrap())
        .collect();
    let id2: Vec<&str> = groups2
        .iter()
        .map(|g| g["media_item_id"].as_str().unwrap())
        .collect();
    assert!(!id1.contains(&id2[0]), "Page 1 and Page 2 must not overlap");
    let eof_data = fetch_gallery_json(
        &app,
        &format!("/api/v1/libraries/{lib_id}/gallery?limit=2&offset=3"),
    )
    .await;
    assert_eq!(
        eof_data.as_array().unwrap().len(),
        0,
        "EOF must be empty array"
    );
}

#[tokio::test]
async fn library_gallery_and_wall_share_filter_and_sort() {
    let tmp = tempfile::tempdir().unwrap();
    let (api_state, app) = gallery_app(&tmp);
    let lib_id = seed_gallery(&tmp, &api_state, &app).await;
    for route in ["items", "gallery"] {
        for filter in ["res=720p", "hdr=true", "rating_gte=9", "w=seen", "d=1990s"] {
            let data = fetch_gallery_json(
                &app,
                &format!("/api/v1/libraries/{lib_id}/{route}?{filter}"),
            )
            .await;
            assert_eq!(
                data.as_array().unwrap().len(),
                0,
                "{route} ignored {filter}"
            );
        }
        for filter in [
            "rating_gte=8&rt=90to120&lang=en",
            "resolutions=1080p&d=2020s",
        ] {
            let data = fetch_gallery_json(
                &app,
                &format!("/api/v1/libraries/{lib_id}/{route}?{filter}"),
            )
            .await;
            let expected = if filter.starts_with("rating") { 1 } else { 3 };
            assert_eq!(
                data.as_array().unwrap().len(),
                expected,
                "{route}: {filter}"
            );
        }
    }
    let wall = fetch_gallery_json(
        &app,
        &format!("/api/v1/libraries/{lib_id}/items?sort=title&order=asc"),
    )
    .await;
    let gallery = fetch_gallery_json(
        &app,
        &format!("/api/v1/libraries/{lib_id}/gallery?sort=title&order=asc"),
    )
    .await;
    let wall_titles: Vec<&str> = wall
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["title"].as_str().unwrap())
        .collect();
    let gallery_titles: Vec<&str> = gallery
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["title"].as_str().unwrap())
        .collect();
    assert_eq!(wall_titles, gallery_titles);
}

#[tokio::test]
async fn library_gallery_skips_empty_artwork_groups() {
    let tmp = tempfile::tempdir().unwrap();
    let (api_state, app) = gallery_app(&tmp);
    let lib_id = seed_gallery(&tmp, &api_state, &app).await;
    let blank_id = MediaId::new();
    api_state
        .store()
        .lock()
        .insert_media(&Media {
            id: blank_id,
            kind: MediaKind::Movie,
            title: "Blank".into(),
            year: Some(2024),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let blank_dir = tmp.path().join("library").join("movies").join("Blank");
    std::fs::create_dir_all(&blank_dir).unwrap();
    let blank_file = blank_dir.join("Blank.mkv");
    std::fs::write(&blank_file, b"content").unwrap();
    api_state
        .store()
        .lock()
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: blank_id,
            path: blank_file.to_string_lossy().to_string(),
            season: None,
            episode: None,
            resolution: Some("1080p".into()),
            codec: Some("H264".into()),
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: domain::Confidence::High,
            filter_score: None,
        })
        .unwrap();
    let gallery = fetch_gallery_json(&app, &format!("/api/v1/libraries/{lib_id}/gallery")).await;
    let titles: Vec<&str> = gallery
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["title"].as_str().unwrap())
        .collect();
    assert!(
        !titles.contains(&"Blank"),
        "empty artwork groups must not consume gallery pages"
    );
}

/// 海报墙能选的每一档排序都必须被服务端接受：未知档回退标题序，不能 400。
#[tokio::test]
async fn library_browse_accepts_every_wall_sort_key() {
    let tmp = tempfile::tempdir().unwrap();
    let (api_state, app) = gallery_app(&tmp);
    let lib_id = seed_gallery(&tmp, &api_state, &app).await;
    for sort in [
        "title",
        "added_at",
        "release_date",
        "release_date_asc",
        "probing",
        "rating",
        "runtime",
        "size",
        "last_played",
        "random",
        "resolution",
    ] {
        for route in ["items", "gallery"] {
            let data = fetch_gallery_json(
                &app,
                &format!("/api/v1/libraries/{lib_id}/{route}?sort={sort}&order=desc"),
            )
            .await;
            assert_eq!(
                data.as_array().unwrap().len(),
                3,
                "{route} rejected sort={sort}"
            );
        }
    }
}

/// 片长与体积排序必须真的排序，而不是静默回退标题序。
#[tokio::test]
async fn library_browse_orders_by_runtime_and_size() {
    let tmp = tempfile::tempdir().unwrap();
    let (api_state, app) = gallery_app(&tmp);
    let lib_id = seed_gallery(&tmp, &api_state, &app).await;
    let first = |data: &Value| {
        data.as_array().unwrap()[0]["title"]
            .as_str()
            .unwrap()
            .to_string()
    };

    // Movie 1 carries <runtime>100</runtime>; the others have no NFO runtime.
    let runtime = fetch_gallery_json(
        &app,
        &format!("/api/v1/libraries/{lib_id}/items?sort=runtime&order=desc"),
    )
    .await;
    assert_eq!(
        first(&runtime),
        "Movie 1",
        "runtime desc must put the longest first"
    );
    let runtime_asc = fetch_gallery_json(
        &app,
        &format!("/api/v1/libraries/{lib_id}/items?sort=runtime&order=asc"),
    )
    .await;
    assert_ne!(
        first(&runtime_asc),
        "Movie 1",
        "runtime asc must not put the longest first"
    );

    // Movie 3's file is the largest by a wide margin.
    let size = fetch_gallery_json(
        &app,
        &format!("/api/v1/libraries/{lib_id}/items?sort=size&order=desc"),
    )
    .await;
    assert_eq!(
        first(&size),
        "Movie 3",
        "size desc must put the largest first"
    );
}

#[tokio::test]
async fn library_gallery_emits_is_favorite_without_explicit_watch_query() {
    let tmp = tempfile::tempdir().unwrap();
    let (api_state, app) = gallery_app(&tmp);
    let lib_id = seed_gallery(&tmp, &api_state, &app).await;

    // Get the first movie's media id
    let gallery_initial =
        fetch_gallery_json(&app, &format!("/api/v1/libraries/{lib_id}/gallery")).await;
    let initial_groups = gallery_initial.as_array().unwrap();
    assert!(!initial_groups.is_empty());
    let target_media_id = initial_groups[0]["media_item_id"].as_str().unwrap();

    // Mark as favorite via marks API
    let mark_resp = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/marks",
            Some("management-secret"),
            serde_json::json!({
                "media_item_id": target_media_id,
                "favorite": true,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(mark_resp.status(), StatusCode::OK);

    // Call gallery without ?w=favorite
    let gallery_after =
        fetch_gallery_json(&app, &format!("/api/v1/libraries/{lib_id}/gallery")).await;
    let groups = gallery_after.as_array().unwrap();
    let matched = groups
        .iter()
        .find(|g| g["media_item_id"].as_str().unwrap() == target_media_id)
        .unwrap();
    assert_eq!(
        matched["is_favorite"], true,
        "Gallery must return true for favorite item without ?w=favorite filter"
    );
}

#[tokio::test]
async fn favorites_view_respects_title_sort_order() {
    let tmp = tempfile::tempdir().unwrap();
    let (api_state, app) = gallery_app(&tmp);
    let _lib_id = seed_gallery(&tmp, &api_state, &app).await;

    let media_ids: std::collections::HashSet<_> = api_state
        .store()
        .lock()
        .list_ledger()
        .unwrap()
        .into_iter()
        .map(|row| row.media_id)
        .collect();

    for media_id in media_ids {
        let resp = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/playback/marks",
                Some("management-secret"),
                serde_json::json!({
                    "media_item_id": media_id.to_string(),
                    "favorite": true,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    let asc_resp =
        fetch_gallery_json(&app, "/api/v1/playback/favorites?sort=title&order=asc").await;
    let titles_asc: Vec<&str> = asc_resp["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles_asc, vec!["Movie 1", "Movie 2", "Movie 3"]);

    let desc_resp =
        fetch_gallery_json(&app, "/api/v1/playback/favorites?sort=title&order=desc").await;
    let titles_desc: Vec<&str> = desc_resp["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles_desc, vec!["Movie 3", "Movie 2", "Movie 1"]);
}

/// P3-NEW-5：gallery 先剔除空组再分页，并且只为本页构建图片 JSON。
#[tokio::test]
async fn library_gallery_expands_only_the_current_page() {
    let tmp = tempfile::tempdir().unwrap();
    let (api_state, app) = gallery_app(&tmp);
    let lib_id = seed_gallery(&tmp, &api_state, &app).await;

    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let api_state = api_state.with_gallery_expansions(counter.clone());
    let app = router(api_state.clone());

    let page1 = fetch_gallery_json(
        &app,
        &format!("/api/v1/libraries/{lib_id}/gallery?limit=1&offset=0"),
    )
    .await;
    assert_eq!(page1.as_array().unwrap().len(), 1);
    assert_eq!(
        counter.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "3 个非空组里 limit=1 只渲染本页 1 组，而不是为全部组构建图片 JSON"
    );

    counter.store(0, std::sync::atomic::Ordering::SeqCst);
    let page2 = fetch_gallery_json(
        &app,
        &format!("/api/v1/libraries/{lib_id}/gallery?limit=1&offset=1"),
    )
    .await;
    assert_eq!(page2.as_array().unwrap().len(), 1);
    assert_eq!(
        counter.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "第二页同样只渲染本页"
    );
}
