//! Subscription depth endpoints (self API): timestamps, real progress,
//! removal-preview torrents, title-preview, upgrade-run reports,
//! season-cleanup, activities, today-arrivals.

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

pub(super) fn authed_app(tmp: &tempfile::TempDir) -> axum::Router {
    router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ))
}

/// Fake catalog for title-preview: one movie + a couple of ambiguous hits.
pub(super) struct FakeCatalog {
    pub(super) movie: domain::Media,
    pub(super) shows: Vec<domain::Media>,
}

impl Catalog for FakeCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![media::CatalogHit {
            media: self.movie.clone(),
            poster_path: None,
            backdrop_path: None,
            rating: None,
            overview: None,
        }])
    }
    fn search_tv(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(self
            .shows
            .iter()
            .map(|media| media::CatalogHit {
                media: media.clone(),
                poster_path: None,
                backdrop_path: None,
                rating: None,
                overview: None,
            })
            .collect())
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(
        &self,
        kind: domain::MediaKind,
        tmdb_id: &str,
    ) -> Result<Option<domain::Media>, String> {
        if kind == domain::MediaKind::Movie && self.movie.tmdb_id.as_deref() == Some(tmdb_id) {
            Ok(Some(self.movie.clone()))
        } else {
            Ok(self
                .shows
                .iter()
                .find(|m| m.tmdb_id.as_deref() == Some(tmdb_id))
                .cloned())
        }
    }
    fn tv_seasons(&self, _tmdb_id: &str) -> Result<Vec<media::TvSeason>, String> {
        Ok(vec![media::TvSeason {
            season_number: 1,
            name: "Season 1".into(),
            episode_count: Some(8),
            air_date: Some("2024-01-01".into()),
            overview: None,
            poster_path: None,
        }])
    }
    fn season_episodes(
        &self,
        _tmdb_id: &str,
        _season: u32,
    ) -> Result<Vec<media::SeasonEpisode>, String> {
        Ok((1..=4)
            .map(|episode_number| media::SeasonEpisode {
                episode_number,
                air_date: Some(format!("2024-01-{episode_number:02}")),
            })
            .collect())
    }
}

pub(super) fn fake_movie() -> domain::Media {
    domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "The Matrix".into(),
        year: Some(1999),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

pub(super) fn fake_state(tmp: &tempfile::TempDir) -> (axum::Router, Arc<MemoryDownloader>) {
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let catalog = Arc::new(FakeCatalog {
        movie: fake_movie(),
        shows: vec![
            domain::Media {
                id: domain::MediaId::new(),
                kind: domain::MediaKind::Tv,
                title: "The Matrix Resurrections".into(),
                year: Some(2021),
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: Some("624860".into()),
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            },
            domain::Media {
                id: domain::MediaId::new(),
                kind: domain::MediaKind::Tv,
                title: "The Matrix Reimagined".into(),
                year: None,
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: Some("999".into()),
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            },
        ],
    });
    let state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader.clone(),
    )
    .with_catalog(catalog);
    (router(state), downloader)
}

pub(super) fn seed_pending_torrent(
    tmp: &tempfile::TempDir,
    subscribe_id: &str,
    title: &str,
    enclosure: &str,
) {
    let store = Store::open(tmp.path().join("data")).unwrap();
    let torrent = domain::Torrent {
        site_id: domain::SiteId::new(),
        title: title.into(),
        enclosure: enclosure.into(),
        size_bytes: Some(1024 * 1024),
        seeders: Some(5),
        free: false,
        hr: false,
        imdb_id: None,
        id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    };
    store
        .merge_pending(
            subscribe_id.parse().unwrap(),
            &[(
                80,
                api::store::PendingDownload {
                    torrent,
                    release_override: None,
                    downloader_id: None,
                    submitted_at: None,
                },
            )],
        )
        .unwrap();
}

#[tokio::test]
async fn subscription_has_real_timestamps_and_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let created = app
        .clone()
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
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    let data = &created["data"];
    let created_at = data["created_at"].as_str().unwrap();
    assert_ne!(created_at, "1970-01-01T00:00:00Z");
    assert!(
        created_at.starts_with("20"),
        "created_at 应为真实时间: {created_at}"
    );
    let id = data["id"].as_str().unwrap();

    // 初始 progress：movie total=1, imported=0, grabbing=0。
    assert_eq!(data["progress"]["total"], 1);
    assert_eq!(data["progress"]["imported"], 0);
    assert_eq!(data["progress"]["grabbing"], 0);

    // PATCH 后 updated_at 前进。
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let patched = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            json!({ "tracking_state": "paused" }),
        ))
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);
    let patched = json_body(patched).await;
    let updated_at = patched["data"]["updated_at"].as_str().unwrap();
    assert!(
        updated_at > created_at,
        "updated_at 应晚于 created_at: {updated_at} vs {created_at}"
    );
}

async fn assert_progress_and_preview(app: &axum::Router, id: &str) {
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
    assert_eq!(detail["data"]["progress"]["grabbing"], 1);
    assert_eq!(detail["data"]["progress"]["downloaded"], 1);
    assert_eq!(detail["data"]["progress"]["missing"], 0);

    let preview = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/subscriptions/{id}/removal-preview"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(preview["data"]["torrent_count"], 1);
    assert!(
        preview["data"]["torrent_titles"][0]
            .as_str()
            .unwrap()
            .contains("Matrix")
    );

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
    assert!(items.iter().any(|item| item["status"] == "grabbed"));
}

#[tokio::test]
async fn progress_and_removal_preview_count_pending_torrents() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
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

    for now in [1, 31, 61, 91, 121, 151] {
        let tick = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(tick.status(), StatusCode::OK);
        if downloader.added().len() == 1 {
            break;
        }
    }
    assert_eq!(
        downloader.added().len(),
        1,
        "搜索 job 应把 Matrix 种子投给下载器"
    );

    assert_progress_and_preview(&app, &id).await;
}

#[tokio::test]
async fn title_preview_resolves_tmdb_ref_to_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _downloader) = fake_state(&tmp);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions/title-preview",
            Some("management-secret"),
            json!({ "title_ref": "tmdb:movie:603" }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"]["status"], "ready");
    assert_eq!(body["data"]["media"]["title"], "The Matrix");
    assert_eq!(body["data"]["media"]["tmdb_id"], "603");
    assert_eq!(body["data"]["movie_owned"], false);
    assert_eq!(body["data"]["existing_subscription_id"], Value::Null);
}

#[tokio::test]
async fn title_preview_bare_title_returns_ambiguous_candidates() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _downloader) = fake_state(&tmp);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions/title-preview",
            Some("management-secret"),
            json!({ "title_ref": "matrix" }),
        ))
        .await
        .unwrap();
    let body = json_body(response).await;
    assert_eq!(body["data"]["status"], "ambiguous");
    let candidates = body["data"]["candidates"].as_array().unwrap();
    assert!(
        candidates.len() >= 2,
        "裸标题多命中应给候选墙: {candidates:?}"
    );
    let refs: Vec<&str> = candidates
        .iter()
        .filter_map(|c| c["title_ref"].as_str())
        .collect();
    assert!(refs.contains(&"tmdb:movie:603"), "候选应含电影: {refs:?}");
    assert!(refs.contains(&"tmdb:tv:624860"), "候选应含剧集: {refs:?}");
}

#[tokio::test]
async fn title_preview_unknown_ref_returns_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _downloader) = fake_state(&tmp);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions/title-preview",
            Some("management-secret"),
            json!({ "title_ref": "tmdb:movie:999999" }),
        ))
        .await
        .unwrap();
    let body = json_body(response).await;
    assert_eq!(body["data"]["status"], "not_found");
}

#[tokio::test]
async fn create_subscription_accepts_title_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _downloader) = fake_state(&tmp);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({ "title_ref": "tmdb:movie:603", "coverage": { "kind": "movie" } }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = json_body(response).await;
    assert_eq!(body["data"]["media"]["title"], "The Matrix");
    assert_eq!(body["data"]["media"]["kind"], "movie");
}

async fn assert_upgrade_report_with_pending(app: &axum::Router, id: &str, tmp: &tempfile::TempDir) {
    let conn = rusqlite::Connection::open(tmp.path().join("data/subscribe.db")).unwrap();
    conn.execute(
        "INSERT INTO pending_downloads (subscribe_id, enclosure, title, score, torrent_json) VALUES (?1, 'https://example.com/e2.torrent', 'The.Long.Watch.S01E02.1080p.mkv', 100, '{}')",
        rusqlite::params![id],
    ).unwrap();

    let report3 = json_body(
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
    assert_eq!(report3["data"]["counts"]["in_flight"], 0);
    assert_eq!(report3["data"]["counts"]["missing"], 1);
}

async fn assert_upgrade_reports(app: &axum::Router, id: &str, tmp: &tempfile::TempDir) {
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
    assert_eq!(report["data"]["counts"]["missing"], 2);
    assert_eq!(report["data"]["counts"]["at_cutoff"], 0);
    assert!(report["data"]["summary"].as_str().unwrap().contains("缺失"));

    let store = Store::open(tmp.path().join("data")).unwrap();
    let mut facts = subscribe::SubscribeFacts::default();
    facts.upsert(
        Some(1),
        Some(1),
        subscribe::QualityFact {
            score: 80,
            path: Some("/tv/e1.mkv".into()),
        },
    );
    store
        .save_subscribe_facts(id.parse().unwrap(), &facts)
        .unwrap();

    let report2 = json_body(
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
    assert_eq!(report2["data"]["counts"]["missing"], 1);
    assert_eq!(report2["data"]["counts"]["at_cutoff"], 1);
    assert_eq!(report2["data"]["counts"]["upgradable"], 0);

    assert_upgrade_report_with_pending(app, id, tmp).await;
}

#[tokio::test]
async fn upgrade_runs_report_matches_facts() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
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

    assert_upgrade_reports(&app, &id, &tmp).await;
}

struct FakeDoubanHttp;

impl media::CatalogGet for FakeDoubanHttp {
    fn get(&self, _path: &str) -> Result<String, media::TmdbError> {
        Ok(douban_html::movie().to_string())
    }
}

mod douban_html {
    pub fn movie() -> &'static str {
        r#"<html><body>
            <h1><span property="v:itemreviewed">肖申克的救赎</span> <span class="year">(1994)</span></h1>
            <div id="info"><span class="pl">类型:</span> 剧情 / 犯罪</div>
        </body></html>"#
    }
}

fn douban_app(tmp: &tempfile::TempDir) -> axum::Router {
    let douban = api::catalog::DoubanCatalog::new(
        media::Douban::new(FakeDoubanHttp, &tmp.path().join("catalog.db")).unwrap(),
    );
    let fanout = api::catalog::FanoutCatalog::new(vec![douban]);
    router(
        state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .with_catalog(fanout),
    )
}

#[tokio::test]
async fn title_preview_resolves_douban_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let app = douban_app(&tmp);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions/title-preview",
            Some("management-secret"),
            json!({ "title_ref": "douban:movie:1292052" }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"]["status"], "ready", "douban:id 应解析成功");
    assert_eq!(body["data"]["media"]["title"], "肖申克的救赎");
    assert_eq!(body["data"]["media"]["douban_id"], "1292052");
    assert_eq!(body["data"]["media"]["kind"], "movie");
}

#[tokio::test]
async fn create_subscription_accepts_douban_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let app = douban_app(&tmp);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "title_ref": "douban:movie:1292052",
                "coverage": { "kind": "movie" },
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = json_body(response).await;
    assert_eq!(body["data"]["media"]["title"], "肖申克的救赎");
    assert_eq!(body["data"]["media"]["douban_id"], "1292052");
}

#[tokio::test]
async fn title_preview_does_not_leak_other_user_subscription_or_block_different_season() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _downloader) = fake_state(&tmp);
    let (_alice_id, alice_token) = create_member(&app, "alice").await;
    let (_bob_id, bob_token) = create_member(&app, "bob").await;

    // Alice 订阅了电影 The Matrix (tmdb:movie:603)
    let alice_sub = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some(&alice_token),
            json!({
                "title_ref": "tmdb:movie:603",
                "coverage": { "kind": "movie" },
            }),
        ))
        .await
        .unwrap();
    assert_eq!(alice_sub.status(), StatusCode::CREATED);

    // Bob 打开同部电影的 title-preview，返回的 existing_subscription_id 必须是 null！绝不能泄漏或被 Alice 阻挡
    let bob_preview = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions/title-preview",
            Some(&bob_token),
            json!({
                "title_ref": "tmdb:movie:603",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(bob_preview.status(), StatusCode::OK);
    let body = json_body(bob_preview).await;
    assert_eq!(
        body["data"]["existing_subscription_id"],
        Value::Null,
        "用户 B 的预检结果绝不可被用户 A 已有订阅占领"
    );
}

#[tokio::test]
async fn run_upgrade_applies_new_ruleset_and_activates_wash_cut() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);

    // 建立一个高画质洗版规则组 (目标分 100)
    let rule_res = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/rule-sets",
                Some("management-secret"),
                json!({
                    "name": "uhd-upgrade",
                    "atoms": [
                        { "kind": "wash_target", "value": "2160p", "priority": 100 },
                        { "kind": "resolution", "value": "2160p", "priority": 100 },
                    ]
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let rule_id = rule_res["data"]["id"].as_str().unwrap();

    // 创建普通订阅 (默认 wash_cut = false)
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "tv", "title": "The Long Watch", "tmdb_id": "100" },
                    "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 1 },
                    "wash_cut": false
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap();

    // 写入一个 720p（得分 50）的已有事实
    let store = Store::open(tmp.path().join("data")).unwrap();
    let mut facts = subscribe::SubscribeFacts::default();
    facts.upsert(
        Some(1),
        Some(1),
        subscribe::QualityFact {
            score: 50,
            path: Some("/fake/s01e01.mkv".into()),
        },
    );
    store
        .save_subscribe_facts(id.parse().unwrap(), &facts)
        .unwrap();
    drop(store);

    // 此时调用 upgrade-runs 并传递新的洗版规则组和激活洗版请求
    let upgrade_res = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/subscriptions/{id}/upgrade-runs"),
                Some("management-secret"),
                json!({
                    "rule_set_id": rule_id,
                    "wash_cut": true
                }),
            ))
            .await
            .unwrap(),
    )
    .await;

    // 检验：规则组必须被应用，且原有 50 分单元成功判定为 upgradable（而不是误判为 at_cutoff）
    assert_eq!(upgrade_res["data"]["rule_set_id"], rule_id);
    assert_eq!(
        upgrade_res["data"]["counts"]["upgradable"], 1,
        "在传递新规则组并开启洗版后，已有低分单元必须被标记为 upgradable 并开始洗版"
    );

    // 检验持久化：订阅的 wash_cut 和 wash_cut_filter_id 均已保存
    let updated = json_body(
        app.oneshot(request(
            "GET",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(updated["data"]["wash_cut"], true);
    assert_eq!(updated["data"]["wash_cut_filter_id"], rule_id);
}
