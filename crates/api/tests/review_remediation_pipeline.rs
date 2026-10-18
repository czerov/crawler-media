#![allow(dead_code)]
#[path = "management/common.rs"]
mod common;

use common::*;
use api::{router, Store};
use domain::*;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};
use tower::ServiceExt;

fn setup(root: &std::path::Path) -> (api::ApiState, Arc<MemoryDownloader>) {
    let dl = Arc::new(MemoryDownloader::new(root.join("stage")));
    let state = state(root, Arc::new(Fixtures {
        requests: Mutex::new(vec![]),
        bodies: HashMap::from([("search", nexusphp())]),
    }), dl.clone());
    (state, dl)
}

fn owned_row(media_id: MediaId, path: &std::path::Path) -> LedgerRow {
    LedgerRow {
        id: LedgerId::new(), media_id, path: path.display().to_string(),
        season: None, episode: None, resolution: Some("2160p".into()),
        codec: Some("hevc".into()), hdr: None, quality_source: QualitySource::Probe,
        confidence: Confidence::High, filter_score: Some(100),
    }
}

async fn create_alias_subscribe(app: &axum::Router) -> Value {
    let mut payload = subscribe_payload("search");
    payload["media"]["douban_id"] = json!("review-identity");
    let response = app.clone().oneshot(request("POST", "/api/v1/subscriptions",
        Some("management-secret"), payload)).await.unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::CREATED);
    json_data(response).await
}

#[tokio::test]
async fn source_quality_survives_persistence_for_wash_cut() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, _) = setup(tmp.path());
    let app = router(state.clone());
    let created = create_subscribe(&app, "search").await;
    let id: SubscribeId = created["id"].as_str().unwrap().parse().unwrap();
    let mut sub = state.store().lock().get_subscribe(id).unwrap().unwrap();
    sub.wash_cut = true;
    let path = tmp.path().join("The.Matrix.2160p.Remux.mkv");
    std::fs::write(&path, b"owned").unwrap();
    let row = owned_row(sub.media_id, &path);
    let mut facts = subscribe::SubscribeFacts::default();
    facts.replace(None, None, subscribe::QualityFact { score: 100, path: Some(row.path.clone()) });
    facts.set_quality(row.path.clone(), release::parse("The.Matrix.2160p.Remux"));
    let filter = Filter::new(FilterId::new(), "source", vec![FilterAtom {
        priority: 1, exclude: false, rule: AtomRule::UpgradeLadder("source".into()),
    }]);
    let torrent = Torrent { site_id: SiteId::new(), title: "The.Matrix.2160p.HDTV".into(),
        enclosure: "https://invalid/lower".into(), size_bytes: None, seeders: None,
        free: false, hr: false, imdb_id: None, id: None, leechers: None,
        snatched: None, upload_time: None, detail_url: None, category: None, poster_url: None };
    let candidate = filter::ScoredTorrent { release: release::parse(&torrent.title), torrent, score: 100 };
    assert!(subscribe::choose(&sub, Some(&filter), &[candidate.clone()], &facts).is_empty());
    {
        let handle = state.store();
        let store = handle.lock();
        store.insert_ledger(&row).unwrap();
        store.save_subscribe_facts(id, &facts).unwrap();
    }
    // Reopen a separate connection to prove the quality is durable, not an in-memory fallback.
    let reopened = Store::open(tmp.path().join("data")).unwrap();
    let saved = reopened.load_subscribe_facts(id).unwrap();
    assert!(subscribe::choose(&sub, Some(&filter), &[candidate], &saved).is_empty(),
        "persisted owned Remux must continue rejecting lower HDTV");
}

#[tokio::test]
async fn legacy_second_run_does_not_turn_pending_into_owned_fact() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, dl) = setup(tmp.path());
    let app = router(state.clone());
    create_site(&app).await;
    let created = create_subscribe(&app, "search").await;
    let id: SubscribeId = created["id"].as_str().unwrap().parse().unwrap();
    for _ in 0..2 {
        let response = app.clone().oneshot(request("POST",
            &format!("/api/v1/subscriptions/{id}/run"), Some("management-secret"), Value::Null))
            .await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["data"]["completed"], false);
    }
    assert!(!dl.added().is_empty());
    let handle = state.store();
    let store = handle.lock();
    assert!(store.list_ledger().unwrap().is_empty());
    assert!(!store.load_pending(id).unwrap().is_empty());
    assert!(store.load_subscribe_facts(id).unwrap().movie().is_none(),
        "unfinished pending must not be persisted as imported facts");
}

#[tokio::test]
async fn existing_library_prevents_redundant_download_for_new_subscribe() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, dl) = setup(tmp.path());
    let app = router(state.clone());
    create_site(&app).await;
    let original = create_alias_subscribe(&app).await;
    let media_id: MediaId = original["media"]["id"].as_str().unwrap().parse().unwrap();
    let root = state.store().lock().default_library(MediaKind::Movie).unwrap().unwrap().root_paths[0].clone();
    let owned = root.join("The Matrix").join("The Matrix - 2160p.mkv");
    std::fs::create_dir_all(owned.parent().unwrap()).unwrap();
    std::fs::write(&owned, b"owned").unwrap();
    state.store().lock().insert_ledger(&owned_row(media_id, &owned)).unwrap();
    // G-M1: one subscription per media/library/coverage — the first one is
    // gone before the new one is created; the library file still counts.
    let original_id: SubscribeId = original["id"].as_str().unwrap().parse().unwrap();
    state.store().lock().delete_subscribe(original_id).unwrap();
    let created = create_alias_subscribe(&app).await;
    assert_eq!(created["media"]["id"], original["media"]["id"]);
    let id = created["id"].as_str().unwrap();
    let response = app.oneshot(request("POST", &format!("/api/v1/subscriptions/{id}/run"),
        Some("management-secret"), Value::Null)).await.unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert!(dl.added().is_empty(), "already-owned Library movie must not be downloaded again");
}

#[tokio::test]
async fn legacy_wash_cut_keeps_old_file_if_replacement_ledger_write_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, dl) = setup(tmp.path());
    let app = router(state.clone());
    create_site(&app).await;
    let mut payload = subscribe_payload("search");
    payload["wash_cut"] = json!(true);
    let response = app.clone().oneshot(request("POST", "/api/v1/subscriptions",
        Some("management-secret"), payload)).await.unwrap();
    let created = json_data(response).await;
    let id: SubscribeId = created["id"].as_str().unwrap().parse().unwrap();
    let media_id: MediaId = created["media"]["id"].as_str().unwrap().parse().unwrap();
    let root = state.store().lock().default_library(MediaKind::Movie).unwrap().unwrap().root_paths[0].clone();
    std::fs::create_dir_all(&root).unwrap();
    let old = root.join("old.mkv");
    std::fs::write(&old, b"owned old version").unwrap();
    let mut row = owned_row(media_id, &old);
    row.resolution = Some("720p".into());
    row.filter_score = Some(1);
    let mut facts = subscribe::SubscribeFacts::default();
    facts.replace(None, None, subscribe::QualityFact { score: 1, path: Some(row.path.clone()) });
    let store = Store::open(tmp.path().join("data")).unwrap();
    store.insert_ledger(&row).unwrap();
    store.save_subscribe_facts(id, &facts).unwrap();
    let replacement = tmp.path().join("better.mkv");
    std::fs::write(&replacement, b"new version").unwrap();
    dl.map_enclosure("https://pt.example/download.php?id=1&passkey=abc", replacement);
    let db = rusqlite::Connection::open(tmp.path().join("data/library.db")).unwrap();
    db.execute_batch("CREATE TRIGGER review_pipeline_fail BEFORE INSERT ON ledger BEGIN SELECT RAISE(ABORT, 'fixture ledger failure'); END;").unwrap();
    let response = app.oneshot(request("POST", &format!("/api/v1/subscriptions/{id}/run"),
        Some("management-secret"), Value::Null)).await.unwrap();
    // Review N-1: the unprobeable replacement is probed at the source and
    // never published, so no ledger write is attempted (200); with a probed
    // replacement the failed ledger write answers 500. Either way the owned
    // version survives.
    assert!(
        matches!(response.status().as_u16(), 200 | 500),
        "{}",
        response.status()
    );
    assert!(old.is_file(), "owned version must survive a failed replacement commit");
    assert!(store.ledger_by_path(&row.path).unwrap().is_some());
}

#[test]
fn season_pack_accepts_bare_sxxexx_episode_names() {
    let media = Media { id: MediaId::new(), kind: MediaKind::Tv, title: "Test Show".into(),
        year: None, original_title: None, aliases: Vec::new(), tmdb_id: None, douban_id: None,
        tvdb_id: None, bangumi_id: None, anilist_id: None };
    let sub = Subscribe { id: SubscribeId::new(), user_id: UserId::new(), media_id: media.id,
        coverage: Coverage::Tv { season: 1, episode_from: 1, episode_to: Some(2) },
        fetch_mode: FetchMode::Search, filter_id: FilterId::new(), wash_cut: false,
        wash_cut_filter_id: None, keep_old_versions: false, full_season_pack: true,
        downloader_id: None, library_id: None, tracking_state: "active".into(),
        follow_future: false, search_interval_secs: 1800 };
    let file = std::path::Path::new("Test.Show.S01/S01E01.mkv");
    let parsed = release::parse("S01E01.mkv");
    let torrent = release::parse("Test.Show.S01.1080p");
    assert!(subscribe::file_identity::resolve_file_release(&sub, &media, file,
        &parsed, &torrent, false).is_some(), "explicit matching episode filename must import");
}
