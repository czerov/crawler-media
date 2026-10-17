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
async fn admin_ended_session_does_not_resurrect_on_subsequent_progress_heartbeat() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "TestMovie".into(),
            year: Some(2023),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("11111".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    let movie_file = tmp.path().join("data/library/movies/TestMovie.mkv");
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
    let device_id = "device-to-end";

    // 1. 发起 start 心跳建立会话
    let start_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/progress",
            Some("management-secret"),
            json!({
                "media_item_id": media.id.to_string(),
                "file_id": row.id.to_string(),
                "device_id": device_id,
                "event": "start",
                "position_ms": 1000,
                "duration_ms": 100000
            }),
        ))
        .await
        .unwrap();
    assert_eq!(start_res.status(), StatusCode::OK);

    // 验证会话在库中存在
    assert!(store.get_session(user_id, device_id).unwrap().is_some());

    // 2. 管理员结束该设备会话
    let end_res = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/playback/activity/sessions/{device_id}/end"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(end_res.status(), StatusCode::OK);

    // 验证会话已被标记为 admin_ended = true
    let s = store.get_session(user_id, device_id).unwrap().unwrap();
    assert!(s.admin_ended);

    // 3. 客户端发送当前会话的普通 progress 心跳，触发服务端收尾并关闭会话
    let progress_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/progress",
            Some("management-secret"),
            json!({
                "media_item_id": media.id.to_string(),
                "file_id": row.id.to_string(),
                "device_id": device_id,
                "event": "timeupdate",
                "position_ms": 2000,
                "duration_ms": 100000
            }),
        ))
        .await
        .unwrap();
    assert_eq!(progress_res.status(), StatusCode::OK);
    let progress_body = json_data(progress_res).await;
    assert_eq!(progress_body["ended_by_admin"], true);

    // 验证会话此时已彻底从 playback_sessions 表中关闭
    assert!(store.get_session(user_id, device_id).unwrap().is_none());

    // 4. 客户端再次发来 progress 心跳，验证系统绝不自动“复活”新会话，且仍返回 ended_by_admin = true (G03)
    let second_progress_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/progress",
            Some("management-secret"),
            json!({
                "media_item_id": media.id.to_string(),
                "file_id": row.id.to_string(),
                "device_id": device_id,
                "event": "timeupdate",
                "position_ms": 3000,
                "duration_ms": 100000
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_progress_res.status(), StatusCode::OK);
    let second_body = json_data(second_progress_res).await;
    assert_eq!(
        second_body["ended_by_admin"], true,
        "已被管理员结束的会话在后续心跳中必须告知客户端已结束"
    );

    assert!(
        store.get_session(user_id, device_id).unwrap().is_none(),
        "已被管理员结束的会话绝不能在后续 progress 心跳中自动复活！(G03)"
    );
}

#[tokio::test]
async fn borrowed_marker_clears_outro_and_does_not_cache_permanently() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "AnimeSeries".into(),
            year: Some(2024),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("22222".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    let ep1_file = tmp.path().join("data/library/tv/AnimeSeries/S01E01.mkv");
    let ep2_file = tmp.path().join("data/library/tv/AnimeSeries/S01E02.mkv");
    std::fs::create_dir_all(ep1_file.parent().unwrap()).unwrap();
    std::fs::write(&ep1_file, b"ep1").unwrap();
    std::fs::write(&ep2_file, b"ep2").unwrap();

    let row1 = LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: ep1_file.display().to_string(),
        season: Some(1),
        episode: Some(1),
        resolution: Some("1080p".into()),
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    let row2 = LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: ep2_file.display().to_string(),
        season: Some(1),
        episode: Some(2),
        resolution: Some("1080p".into()),
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row1).unwrap();
    store.insert_ledger(&row2).unwrap();

    // 在 Episode 1 写入完整的片头和片尾标记
    store
        .put_media_marker(&store::StoredMediaMarker {
            media_id: media.id,
            season: 1,
            episode: 1,
            intro_start_ms: Some(10_000),
            intro_end_ms: Some(100_000),
            outro_start_ms: Some(1_300_000),
            outro_end_ms: Some(1_400_000),
            source: "theintrodb".into(),
            locked: false,
            updated_at: 1000,
        })
        .unwrap();
    // Episode 2 写入仅包含空 intro_start_ms 的初始缓存 (is_only_intro 状态)
    store
        .put_cached_chapters(
            &row2.id.to_string(),
            &[marker::ChapterMarker {
                start_ms: 0,
                end_ms: 10_000,
                title: Some("Intro".into()),
                marker_type: Some(marker::MarkerType::IntroStart),
                synthetic: false,
            }],
        )
        .unwrap();
    drop(store);

    // 请求 Episode 2 的章节信息
    let chapters_res = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/playback/chapters?file_id={}", row2.id),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(chapters_res.status(), StatusCode::OK);
    let chapters_body = json_data(chapters_res).await;
    let chapters = chapters_body["chapters"].as_array().unwrap();

    // 验证：借用的章节必须只包含片头 (intro)，绝对不借用 ep1 的片尾 (outro) (G08)
    let has_outro = chapters.iter().any(|c| {
        c["title"].as_str().unwrap_or_default().contains("Outro")
            || c["title"].as_str().unwrap_or_default().contains("片尾")
            || c["start_ms"] == 1_300_000
    });
    assert!(
        !has_outro,
        "借用其他集的 marker 绝对不能借用片尾 outro 绝对时间！(G08)"
    );

    // 验证：借用生成的内容绝不被永久写入数据库 put_cached_chapters，保留自愈能力 (G08)
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let ep2_persisted = store
        .get_cached_chapters(&row2.id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(ep2_persisted.len(), 1, "借用的暂态章节标记不应永久覆盖缓存");
}
