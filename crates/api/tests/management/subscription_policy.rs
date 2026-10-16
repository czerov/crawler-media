use std::collections::HashMap;
use std::sync::Arc;

use api::catalog::Catalog;
use api::{Store, router};
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;
use super::subscription_depth::{authed_app, fake_state};

async fn create_long_watch_sub(app: &axum::Router) -> String {
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "tv", "title": "The Long Watch", "tmdb_id": "100" },
                    "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    created["data"]["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn patch_selected_seasons_adjusts_tv_coverage() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let id = create_long_watch_sub(&app).await;

    // 减季：只保留第 2 季。
    let patched = json_body(
        app.clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/v1/subscriptions/{id}"),
                Some("management-secret"),
                json!({ "selected_seasons": [2] }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        patched["data"]["coverage"]["season"], 2,
        "季调整应改 coverage"
    );
    assert_eq!(patched["data"]["coverage"]["episode_from"], 1);
    // 列表回读一致。
    let listed = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/subscriptions/{id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(listed["data"]["coverage"]["season"], 2);

    let multiple = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            json!({ "selected_seasons": [2, 3] }),
        ))
        .await
        .unwrap();
    assert_eq!(multiple.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// 订阅缺口补齐：手动选种 units、订阅完成标记、wash_target 原子。
// ---------------------------------------------------------------------------

#[tokio::test]
async fn manual_grab_reports_covered_units() {
    let tmp = tempfile::tempdir().unwrap();
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader.clone(),
    ));
    let response = app
        .oneshot(request(
            "POST",
            "/api/v1/downloaders/submit",
            Some("management-secret"),
            json!({
                "download_url": "https://pt.example/dl/1",
                "title": "Show.2024.S01E03-E04.1080p.WEB-DL",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let units = body["data"]["units"].as_array().unwrap();
    assert_eq!(units.len(), 2, "S01E03-E04 应覆盖 2 个单元");
    assert_eq!(units[0]["season_number"], 1);
    assert_eq!(units[0]["episode_number"], 3);
    assert_eq!(units[1]["episode_number"], 4);
}

async fn poll_for_imported(app: &axum::Router, id: &str) -> bool {
    for now in [1, 31, 61, 91, 121, 151, 181, 211, 241, 271, 301] {
        let _ = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await;
        let detail = json_body(
            app.clone()
                .oneshot(request(
                    "GET",
                    &format!("/api/v1/subscriptions/{id}"),
                    Some("management-secret"),
                    Value::Null,
                ))
                .await
                .unwrap(),
        )
        .await;
        if detail["data"]["progress"]["imported"] == 1 {
            return true;
        }
    }
    false
}

#[tokio::test]
async fn completed_subscribe_marks_setting_once() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let source = tmp.path().join("The.Matrix.1999.1080p.mkv");
    std::fs::write(&source, b"movie-bytes").unwrap();
    downloader.map_enclosure("https://pt.example/download.php?id=1&passkey=abc", source);
    let app = router(state(tmp.path(), fetcher, downloader.clone()));
    create_site(&app).await;
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "movie", "title": "The Matrix" },
                    "coverage": { "kind": "movie" },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap().to_string();

    assert!(poll_for_imported(&app, &id).await, "搜索+转移后应入库完成");
    let store = Store::open(tmp.path().join("data")).unwrap();
    let key = format!("subscribe.completed:{id}");
    assert!(
        store.get_setting(&key).unwrap().is_some(),
        "订阅全部入库后应写完成标记"
    );
}

#[tokio::test]
async fn rule_sets_accept_wash_target_atom_and_wash_target_prefers_it() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/rule-sets",
                Some("management-secret"),
                json!({
                    "name": "wash",
                    "atoms": [
                        { "kind": "resolution", "value": "1080p", "priority": 50 },
                        { "kind": "wash_target", "value": "2160p Remux", "priority": 100 },
                    ],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let filter_id = created["data"]["id"].as_str().unwrap().to_string();
    // 订阅挂该规则组，开洗版 → upgrade-runs 目标标签应为显式 WashTarget。
    let sub = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "tv", "title": "The Long Watch", "tmdb_id": "100" },
                    "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 1 },
                    "filter_id": filter_id,
                    "wash_cut": true,
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = sub["data"]["id"].as_str().unwrap().to_string();
    let report = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/subscriptions/{id}/upgrade-runs"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        report["data"]["target_label"], "2160p Remux",
        "显式 wash_target 应优先于 resolution 推导"
    );
}

#[tokio::test]
async fn patch_keep_old_versions_flips_subscribe() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "movie", "title": "The Matrix" },
                    "coverage": { "kind": "movie" },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["data"]["keep_old_versions"], false);

    let patched = json_body(
        app.clone()
            .oneshot(request(
                "PATCH",
                &format!("/api/v1/subscriptions/{id}"),
                Some("management-secret"),
                json!({ "keep_old_versions": true }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        patched["data"]["keep_old_versions"], true,
        "PATCH 应翻转 keep_old_versions"
    );
    let detail = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/subscriptions/{id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(detail["data"]["keep_old_versions"], true, "详情回读一致");
}

#[tokio::test]
async fn wanted_tv_units_carry_air_date_and_forecast() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _downloader) = fake_state(&tmp);
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "tv", "title": "The Matrix Resurrections", "tmdb_id": "624860" },
                    "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap().to_string();

    let detail = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/subscriptions/{id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let wanted = detail["data"]["wanted"].as_array().unwrap();
    assert_eq!(wanted.len(), 2);
    assert_eq!(wanted[0]["air_date"], "2024-01-01", "TV 单元应带播出日期");
    assert_eq!(wanted[1]["air_date"], "2024-01-02");
}

#[tokio::test]
async fn download_routing_preview_returns_real_placement() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions/download-routing-preview",
            Some("management-secret"),
            json!({ "kind": "movie", "title": "The Matrix", "year": 1999 }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let data = &body["data"];
    // 默认库存在（seed_defaults 创建 movie/tv 库）。
    assert!(data["library_id"].is_string(), "应解析出目标媒体库: {data}");
    assert!(!data["library_name"].as_str().unwrap_or("").is_empty());
    assert!(data["entry_dir"].is_string(), "应按命名模板渲染条目目录");
    assert!(data["entry_dir"].as_str().unwrap().contains("The Matrix"));
    assert_eq!(data["mode"], "downloader_default");
    // 无默认下载器 → 未就绪提示（ok=false 但结构化）。
    assert_eq!(data["ok"], false);
    assert!(data["warning"].is_string());
}

fn setup_ladder_and_old_ledger(tmp: &tempfile::TempDir, media_id: &str, sub_id: &str) {
    let old = tmp.path().join("data/library/movies/Movie.2024.1080p.mkv");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, b"old-1080p").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .insert_ledger(&domain::LedgerRow {
            id: domain::LedgerId::new(),
            media_id: media_id.parse().unwrap(),
            path: old.display().to_string(),
            season: None,
            episode: None,
            resolution: Some("1080p".into()),
            codec: Some("h264".into()),
            hdr: None,
            quality_source: domain::QualitySource::Probe,
            confidence: domain::Confidence::High,
            filter_score: Some(200),
        })
        .unwrap();
    let mut facts = subscribe::SubscribeFacts::default();
    facts.upsert(
        None,
        None,
        subscribe::QualityFact {
            score: 200,
            path: Some(old.display().to_string()),
        },
    );
    store
        .save_subscribe_facts(sub_id.parse().unwrap(), &facts)
        .unwrap();
}

async fn create_ladder_filter_and_sub(app: &axum::Router) -> (String, String) {
    let rule = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/rule-sets",
                Some("management-secret"),
                json!({
                    "name": "ladder",
                    "atoms": [
                        { "kind": "resolution", "value": "2160p", "priority": 100 },
                        { "kind": "upgrade_ladder", "value": "resolution", "priority": 50 },
                    ],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let filter_id = rule["data"]["id"].as_str().unwrap().to_string();

    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "movie", "title": "The Matrix" },
                    "coverage": { "kind": "movie" },
                    "filter_id": filter_id,
                    "wash_cut": true,
                    "wash_cut_filter_id": filter_id,
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    (
        created["data"]["id"].as_str().unwrap().to_string(),
        created["data"]["media"]["id"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn upgrade_ladder_replaces_even_when_score_is_lower() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader.clone()));
    create_site(&app).await;

    let (id, media_id) = create_ladder_filter_and_sub(&app).await;
    setup_ladder_and_old_ledger(&tmp, &media_id, &id);

    for now in [1, 31, 61, 91, 121, 151] {
        app.clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        if downloader.added().len() == 1 {
            break;
        }
    }
    assert_eq!(
        downloader.added().len(),
        1,
        "ladder 判定 2160p 优于 1080p，分数更低也应替换"
    );
    assert!(downloader.added()[0].title.contains("2160p"));
}

async fn manual_grab_torrent(app: &axum::Router, id: &str) -> Value {
    json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/downloaders/submit",
                Some("management-secret"),
                json!({
                    "subscribe_id": id,
                    "download_url": "https://pt.example/dl/1",
                    "title": "The.Long.Watch.2024.S01E01.1080p.WEB-DL",
                }),
            ))
            .await
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn manual_grab_with_subscribe_id_enters_subscription_pipeline() {
    let tmp = tempfile::tempdir().unwrap();
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader,
    ));
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "tv", "title": "The Long Watch", "tmdb_id": "100" },
                    "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap().to_string();

    let grabbed = manual_grab_torrent(&app, &id).await;
    assert_eq!(grabbed["data"]["ok"], true);
    assert_eq!(grabbed["data"]["units"][0]["episode_number"], 1);

    let detail = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/subscriptions/{id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let wanted = detail["data"]["wanted"].as_array().unwrap();
    assert_eq!(
        wanted[0]["status"], "grabbed",
        "手动选种应物化 grabbed 工单"
    );
    assert!(
        wanted[0]["grab_title"]
            .as_str()
            .unwrap()
            .contains("Long.Watch")
    );
    assert!(wanted[0]["resource_timing"]["submitted_at"].is_string());
}

fn make_today_arrivals_app(tmp: &tempfile::TempDir) -> axum::Router {
    let today = api::store::now_rfc3339();
    let tomorrow = {
        let mut parts: Vec<i64> = today
            .split(['-', 'T'])
            .filter_map(|p| p.parse().ok())
            .collect();
        parts[2] += 1;
        format!("{:04}-{:02}-{:02}", parts[0], parts[1], parts[2])
    };
    struct TodayCatalog {
        air_date: String,
    }
    impl Catalog for TodayCatalog {
        fn search_movie(&self, _q: &str) -> Result<Vec<media::CatalogHit>, String> {
            Ok(vec![])
        }
        fn search_tv(&self, _q: &str) -> Result<Vec<media::CatalogHit>, String> {
            Ok(vec![])
        }
        fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
            Ok(vec![])
        }
        fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
            Ok(vec![])
        }
        fn details(
            &self,
            _k: domain::MediaKind,
            _i: &str,
        ) -> Result<Option<domain::Media>, String> {
            Ok(None)
        }
        fn season_episodes(&self, _i: &str, _s: u32) -> Result<Vec<media::SeasonEpisode>, String> {
            Ok(vec![media::SeasonEpisode {
                episode_number: 1,
                air_date: Some(self.air_date.clone()),
            }])
        }
    }
    let catalog = Arc::new(TodayCatalog { air_date: tomorrow });
    router(
        state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .with_catalog(catalog),
    )
}

#[tokio::test]
async fn today_arrivals_carry_release_forecast_for_upcoming_episodes() {
    let tmp = tempfile::tempdir().unwrap();
    let app = make_today_arrivals_app(&tmp);

    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "tv", "title": "The Long Watch", "tmdb_id": "100" },
                    "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let _ = created["data"]["id"].as_str().unwrap();

    let arrivals = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/subscriptions/today-arrivals",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let items = arrivals["data"].as_array().unwrap();
    let with_forecast = items.iter().find(|i| i["release_forecast"].is_object());
    assert!(
        with_forecast.is_some(),
        "应有带 release_forecast 的 arrival: {items:?}"
    );
    assert!(with_forecast.unwrap()["release_forecast"]["target_air_date"].is_string());
}

#[tokio::test]
async fn l3_upgrade_ladder_rejects_unknown_dimensions() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let save = |ladder: &'static str| {
        let app = app.clone();
        async move {
            app.oneshot(request(
                "POST",
                "/api/v1/rule-sets",
                Some("management-secret"),
                json!({
                    "name": "ladder",
                    "atoms": [{ "kind": "upgrade_ladder", "value": ladder, "priority": 50 }],
                }),
            ))
            .await
            .unwrap()
            .status()
        }
    };
    assert_eq!(save("resolutions").await, StatusCode::BAD_REQUEST);
    assert_eq!(save("resolution, hdr").await, StatusCode::CREATED);
}
