use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
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

#[tokio::test]
async fn tv_with_season_favorite_appears_in_library_favorites_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    // 1. 创建 TV 剧集文件和台账
    let tv_dir = tmp.path().join("data/library/tv/TestShow/Season 01");
    std::fs::create_dir_all(&tv_dir).unwrap();
    let ep_path = tv_dir.join("TestShow.S01E01.mkv");
    std::fs::write(&ep_path, b"video").unwrap();

    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "TestShow".into(),
            year: Some(2024),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("99999".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: media.id,
            path: ep_path.display().to_string(),
            season: Some(1),
            episode: Some(1),
            resolution: Some("1080p".into()),
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();

    // 找到 TV 库 id
    let tv_lib = store
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|l| l.kind == MediaKind::Tv)
        .unwrap();
    let user_id = store.admin_user_id().unwrap();

    // 2. 仅在第 1 季第 1 集标记收藏 (S01E01)
    store
        .upsert_unit(
            user_id,
            media.id,
            1,
            1,
            0,
            None,
            Some(true), // favorite = true
            None,
            None,
            None,
            false,
            1000,
        )
        .unwrap();
    drop(store);

    // 3. 查询 /libraries/{id}/items?filter=favorites
    let res = app
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{}/items?filter=favorites", tv_lib.id),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = json_data(res).await;
    let items = body.as_array().unwrap();
    assert_eq!(
        items.len(),
        1,
        "分级收藏（单集收藏）的作品必须出现在媒体库收藏列表中 (F12)"
    );
    assert_eq!(items[0]["title"], "TestShow");
}

#[tokio::test]
async fn playback_heartbeat_at_90_percent_sets_played_true() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "ShortFilm".into(),
            year: Some(2024),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("88888".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    let movie_file = tmp.path().join("data/library/movies/ShortFilm.mkv");
    std::fs::create_dir_all(movie_file.parent().unwrap()).unwrap();
    std::fs::write(&movie_file, b"video").unwrap();

    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: movie_file.display().to_string(),
        season: None,
        episode: None,
        resolution: Some("1080p".into()),
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).unwrap();

    let user_id = store.admin_user_id().unwrap();
    drop(store);

    // 发送心跳：总时长 100,000ms，当前进度 95,000ms (95%)
    let heartbeat_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/progress",
            Some("management-secret"),
            json!({
                "media_item_id": media.id.to_string(),
                "file_id": row.id.to_string(),
                "device_id": "test-device",
                "event": "timeupdate",
                "position_ms": 95000,
                "duration_ms": 100000
            }),
        ))
        .await
        .unwrap();
    assert_eq!(heartbeat_res.status(), StatusCode::OK);

    // 验证数据库中的 unit 已自动标记为 played = true (G01)
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let unit = store
        .unit_state(user_id, media.id, -1, -1)
        .unwrap()
        .unwrap();
    assert!(
        unit.played,
        "播放到达 90% 阈值后必须自动置 played = true (G01)"
    );
}

#[tokio::test]
async fn up_next_movie_dto_reads_canonical_unit_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let movie_dir = tmp.path().join("data/library/movies/Dune");
    std::fs::create_dir_all(&movie_dir).unwrap();
    let movie_file = movie_dir.join("Dune.2021.mkv");
    std::fs::write(&movie_file, b"video").unwrap();

    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "Dune".into(),
            year: Some(2021),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("438631".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: media.id,
            path: movie_file.display().to_string(),
            season: None,
            episode: None,
            resolution: Some("2160p".into()),
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();

    let user_id = store.admin_user_id().unwrap();

    // 在规范单位 (-1, -1) 写入观看到一半的进度 (50,000ms / 120,000ms)
    store
        .upsert_unit(
            user_id,
            media.id,
            -1,
            -1,
            50000,
            Some(false),
            None,
            Some(120000),
            None,
            None,
            false,
            2000,
        )
        .unwrap();
    drop(store);

    // 查询 /playback/up-next
    let res = app
        .oneshot(request(
            "GET",
            "/api/v1/playback/up-next",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = json_data(res).await;
    let items = body["items"].as_array().unwrap();
    let dune_item = items
        .iter()
        .find(|it| it["title"] == "Dune")
        .expect("Dune 应该在 up_next 列表中");

    assert_eq!(
        dune_item["position_ms"], 50000,
        "up_next 电影 DTO 必须正确读取规范单位的进度，不能是 0 (G02)"
    );
}
