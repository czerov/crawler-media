//! B3 播放读路径批量化回归：collection 真实收藏态、favorites 排序键、activity 字段、
//! 以及同一 Media 跨多个 Library 时 `library_id` 仍为 null。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::Request;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, UserRole};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::Value;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn make_state(root: &Path) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
}

async fn json_data(response: axum::response::Response) -> Value {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["ok"], true, "status={status} body={body}");
    body.get("data").cloned().unwrap_or(Value::Null)
}

async fn get(app: &axum::Router, uri: &str, token: &str) -> Value {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    json_data(app.clone().oneshot(request).await.unwrap()).await
}

fn seed_member(store: &Store, login: &str) -> (domain::UserId, String) {
    let id = domain::UserId::new();
    store
        .insert_user_with_password(
            &domain::User {
                id,
                login: login.into(),
                enabled: true,
                role: UserRole::Member,
            },
            "member-pass",
        )
        .unwrap();
    let token = format!("{login}-session");
    store.set_user_token(id, &token).unwrap();
    (id, token)
}

fn seed_library(store: &Store, root: &Path, name: &str) -> PathBuf {
    let library_root = root.join(format!("library-{name}"));
    std::fs::create_dir_all(&library_root).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            name,
            &[library_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    library_root
}

fn seed_media(store: &Store, title: &str, year: Option<u16>) -> MediaId {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.into(),
        year,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    media.id
}

fn add_ledger(store: &Store, media_id: MediaId, library_root: &Path, name: &str) {
    let path = library_root.join(format!("{name}.mkv"));
    std::fs::write(&path, b"x").unwrap();
    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id,
            path: path.display().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();
}

fn favorite(store: &Store, user_id: domain::UserId, media_id: MediaId, favorite: bool) {
    store
        .set_unit_marks(
            user_id,
            media_id,
            api::store::UNIT_WHOLE,
            api::store::UNIT_WHOLE,
            None,
            Some(favorite),
            now_secs(),
        )
        .unwrap();
}

#[tokio::test]
async fn collection_gallery_reports_real_favorite_state() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let library = seed_library(&store.lock(), tmp.path(), "collections");
    let (member, token) = seed_member(&store.lock(), "alice");
    let (favorited, plain, collection) = {
        let guard = store.lock();
        let favorited = seed_media(&guard, "Favorited", Some(2020));
        let plain = seed_media(&guard, "Plain", Some(2021));
        add_ledger(&guard, favorited, &library, "Favorited");
        add_ledger(&guard, plain, &library, "Plain");
        let collection = guard.create_collection(member, "混装").unwrap();
        guard
            .add_collection_item(&collection, &favorited.to_string())
            .unwrap();
        guard
            .add_collection_item(&collection, &plain.to_string())
            .unwrap();
        (favorited, plain, collection)
    };
    favorite(&store.lock(), member, favorited, true);

    let data = get(
        &app,
        &format!("/api/v1/collections/{collection}/gallery"),
        &token,
    )
    .await;
    let groups = data.as_array().unwrap();
    assert_eq!(groups.len(), 2, "{data}");
    let by_id = |id: MediaId| {
        groups
            .iter()
            .find(|group| group["media_item_id"] == id.to_string())
            .unwrap_or_else(|| panic!("missing {id}: {data}"))
            .clone()
    };
    assert_eq!(
        by_id(favorited)["is_favorite"],
        true,
        "favorited item must report true: {data}"
    );
    assert_eq!(
        by_id(plain)["is_favorite"],
        false,
        "unfavorited item must not be hardcoded true: {data}"
    );
}

#[tokio::test]
async fn favorites_sort_matches_existing_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let library = seed_library(&store.lock(), tmp.path(), "favorites");
    let (member, token) = seed_member(&store.lock(), "bob");
    let (alpha_old, alpha_new, beta, gamma) = {
        let guard = store.lock();
        let alpha_old = seed_media(&guard, "Alpha", Some(2019));
        let alpha_new = seed_media(&guard, "Alpha", Some(2024));
        let beta = seed_media(&guard, "Beta", Some(2021));
        let gamma = seed_media(&guard, "Gamma", Some(2022));
        for (id, name) in [
            (alpha_old, "alpha-old"),
            (alpha_new, "alpha-new"),
            (beta, "beta"),
            (gamma, "gamma"),
        ] {
            add_ledger(&guard, id, &library, name);
            favorite(&guard, member, id, true);
        }
        (alpha_old, alpha_new, beta, gamma)
    };

    let by_title = get(
        &app,
        "/api/v1/playback/favorites?sort=title&order=asc&limit=10",
        &token,
    )
    .await;
    let titles: Vec<&str> = by_title["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        vec!["Alpha", "Alpha", "Beta", "Gamma"],
        "{by_title}"
    );

    // 同年份同标题时用 id 升序兜底（stable tie-break）。
    let alpha_ids = by_title["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["title"] == "Alpha")
        .map(|item| item["media_item_id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    let mut expected = vec![alpha_new.to_string(), alpha_old.to_string()];
    expected.sort();
    assert_eq!(
        alpha_ids, expected,
        "same-key tie-break must stay id-ascending"
    );

    let by_year = get(
        &app,
        "/api/v1/playback/favorites?sort=year&order=asc&limit=10",
        &token,
    )
    .await;
    let years: Vec<u64> = by_year["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["year"].as_u64().unwrap())
        .collect();
    assert_eq!(years, vec![2019, 2021, 2022, 2024], "{by_year}");
    assert_eq!(by_year["total"], 4);
    let _ = (beta, gamma);
}

#[tokio::test]
async fn activity_media_target_fields_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let library = seed_library(&store.lock(), tmp.path(), "activity");
    let (member, token) = seed_member(&store.lock(), "carol");
    let media = {
        let guard = store.lock();
        let media = seed_media(&guard, "Watching", Some(2023));
        add_ledger(&guard, media, &library, "Watching");
        guard
            .upsert_session(&api::store::SessionRow {
                device_id: "dev-1".into(),
                user_id: member,
                media_id: media,
                season: None,
                episode: None,
                client: Some("test".into()),
                device_name: Some("test".into()),
                client_version: None,
                play_method: "local".into(),
                position_ms: 50,
                start_position_ms: 0,
                duration_ms: Some(100),
                paused: false,
                watched_ms: 50,
                rate_bps: 0,
                bytes_sent: 0,
                connections: 1,
                admin_ended: false,
                started_at: now_secs(),
                last_report_at: now_secs(),
                last_report_at_ms: now_secs() * 1000,
                ledger_id: None,
                play_session_id: None,
                last_event_sequence: None,
            })
            .unwrap();
        media
    };

    let data = get(&app, "/api/v1/playback/activity", &token).await;
    assert_eq!(data["hidden_session_count"], 0, "{data}");
    let sessions = data["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{data}");
    let session = &sessions[0];
    assert_eq!(session["member_name"], "carol");
    assert_eq!(session["user_id"], member.to_string());
    assert_eq!(session["device_id"], "dev-1");
    assert_eq!(session["position_ms"], 50);
    assert_eq!(session["progress_percent"], 50);
    let target = &session["media"];
    assert_eq!(target["media_item_id"], media.to_string());
    assert_eq!(target["title"], "Watching");
    assert_eq!(target["kind"], "movie");
    assert_eq!(target["year"], 2023);
    assert_eq!(target["browsable"], true);
    assert_eq!(target["season_number"], 0);
    assert_eq!(target["episode_number"], 0);
    assert_eq!(target["episode_title"], Value::Null);
    let library_id = target["library_id"].as_str().unwrap();
    assert!(
        !library_id.is_empty(),
        "owned single library must be reported"
    );
}

#[tokio::test]
async fn media_in_two_libraries_reports_null_library_id() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let first = seed_library(&store.lock(), tmp.path(), "first");
    let second = seed_library(&store.lock(), tmp.path(), "second");
    let (member, token) = seed_member(&store.lock(), "dave");
    let media = {
        let guard = store.lock();
        let media = seed_media(&guard, "Shared", Some(2020));
        add_ledger(&guard, media, &first, "Shared-A");
        add_ledger(&guard, media, &second, "Shared-B");
        guard
            .upsert_session(&api::store::SessionRow {
                device_id: "dev-2".into(),
                user_id: member,
                media_id: media,
                season: None,
                episode: None,
                client: None,
                device_name: None,
                client_version: None,
                play_method: "local".into(),
                position_ms: 0,
                start_position_ms: 0,
                duration_ms: None,
                paused: false,
                watched_ms: 0,
                rate_bps: 0,
                bytes_sent: 0,
                connections: 1,
                admin_ended: false,
                started_at: now_secs(),
                last_report_at: now_secs(),
                last_report_at_ms: now_secs() * 1000,
                ledger_id: None,
                play_session_id: None,
                last_event_sequence: None,
            })
            .unwrap();
        media
    };

    let data = get(&app, "/api/v1/playback/activity", &token).await;
    let sessions = data["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{data}");
    let target = &sessions[0]["media"];
    assert_eq!(target["media_item_id"], media.to_string());
    assert_eq!(
        target["library_id"],
        Value::Null,
        "ambiguous ownership must stay null: {data}"
    );
}
