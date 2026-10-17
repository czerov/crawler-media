//! 取消订阅的级联清理：delete_library_files=true 时该媒体库内文件移入回收站。

use std::collections::HashMap;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use domain::{
    Confidence, Coverage, FetchMode, Filter, FilterId, LedgerId, LedgerRow, Media, MediaId,
    MediaKind, QualitySource, Subscribe, SubscribeId, UserId,
};
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
async fn cancel_subscription_with_files_bins_library_files() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let movie = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&movie).unwrap();
    let file = movie.join("The.Matrix.1999.2160p.mkv");
    std::fs::write(&file, b"matrix").unwrap();

    // store 直接造数：media + ledger + subscribe + filter。
    let store = Store::open(tmp.path().join("data")).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: Some(1999),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: media.id,
            path: file.display().to_string(),
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
    let filter = Filter {
        id: FilterId::new(),
        name: "f".into(),
        atoms: vec![],
        keep_old_versions: false,
    };
    store.insert_filter(&filter).unwrap();
    let subscribe = Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: media.id,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: filter.id,
        wash_cut: false,
        keep_old_versions: false,
        wash_cut_filter_id: None,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };
    store.insert_subscribe(&subscribe).unwrap();
    drop(store);

    let deleted = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!(
                "/api/v1/subscriptions/{}?delete_library_files=true",
                subscribe.id
            ),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);

    // 库文件直接物理删除、台账清空。
    assert!(
        !std::path::Path::new(&file).exists(),
        "original file removed"
    );
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
    assert!(
        ledger["data"].as_array().unwrap().is_empty(),
        "ledger cleared"
    );
}

fn tv_media() -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "The Long Watch".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn tv_subscribe(media_id: MediaId, season: u32) -> Subscribe {
    Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id,
        coverage: Coverage::Tv {
            season,
            episode_from: 1,
            episode_to: Some(1),
        },
        fetch_mode: FetchMode::Search,
        filter_id: FilterId::new(),
        wash_cut: false,
        keep_old_versions: false,
        wash_cut_filter_id: None,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    }
}

fn insert_episode(store: &Store, media_id: MediaId, path: &std::path::Path, season: u32) {
    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id,
            path: path.display().to_string(),
            season: Some(season),
            episode: Some(1),
            resolution: Some("1080p".into()),
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();
}

#[tokio::test]
async fn cancel_season_one_does_not_delete_other_season_or_library_files() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let tv_a = tmp.path().join("data/library/tv");
    let tv_b = tmp.path().join("private-tv");
    std::fs::create_dir_all(&tv_a).unwrap();
    std::fs::create_dir_all(&tv_b).unwrap();
    let s1 = tv_a.join("S01E01.mkv");
    let s2 = tv_a.join("S02E01.mkv");
    let other_lib = tv_b.join("S01E01.mkv");
    std::fs::write(&s1, b"s1").unwrap();
    std::fs::write(&s2, b"s2").unwrap();
    std::fs::write(&other_lib, b"other").unwrap();

    let created_lib = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                serde_json::json!({
                    "name": "私有剧集",
                    "kind": "tv",
                    "root_paths": [tv_b.display().to_string()],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let other_library_id = created_lib["data"]["id"].as_str().unwrap().to_string();

    let store = Store::open(tmp.path().join("data")).unwrap();
    let media = tv_media();
    store.insert_media(&media).unwrap();
    let filter = Filter {
        id: FilterId::new(),
        name: "f".into(),
        atoms: vec![],
        keep_old_versions: false,
    };
    store.insert_filter(&filter).unwrap();
    insert_episode(&store, media.id, &s1, 1);
    insert_episode(&store, media.id, &s2, 2);
    insert_episode(&store, media.id, &other_lib, 1);
    let mut season_one = tv_subscribe(media.id, 1);
    season_one.filter_id = filter.id;
    let mut season_two = tv_subscribe(media.id, 2);
    season_two.filter_id = filter.id;
    store.insert_subscribe(&season_one).unwrap();
    store.insert_subscribe(&season_two).unwrap();
    drop(store);

    let deleted = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!(
                "/api/v1/subscriptions/{}?delete_library_files=true",
                season_one.id
            ),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    assert!(!s1.exists(), "本季文件应删除");
    assert!(s2.exists(), "其他季文件绝不能被带走");
    assert!(
        other_lib.exists(),
        "其他 Library 的同 Media 文件绝不能被带走"
    );
    let store = Store::open(tmp.path().join("data")).unwrap();
    let remaining = store.ledger_for_media(media.id).unwrap();
    assert_eq!(remaining.len(), 2);
    assert!(remaining.iter().any(|row| row.season == Some(2)));
    assert!(
        remaining
            .iter()
            .any(|row| row.path == other_lib.display().to_string())
    );
    let _ = other_library_id;
}
