//! B5b：播放历史接入 SQL 可见范围分页。
//!
//! characterization：entries/hidden_count/has_more/复合游标/days/scope=all 语义不变；
//! 另有一条真实回归：hidden_count 必须覆盖整个 scope，而不是旧的「前 20 万行」窗口。

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

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

async fn json_data(response: axum::response::Response) -> Value {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["ok"], true, "status={status} body={body}");
    body.get("data").cloned().unwrap_or(Value::Null)
}

async fn history(app: &axum::Router, token: &str, query: &str) -> Value {
    let uri = if query.is_empty() {
        "/api/v1/playback/history".to_string()
    } else {
        format!("/api/v1/playback/history?{query}")
    };
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

/// 成员可见的媒体库（access_mode=everyone）。
fn seed_visible_library(store: &Store, root: &Path) -> PathBuf {
    let library_root = root.join("visible-library");
    std::fs::create_dir_all(&library_root).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Visible",
            &[library_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    library_root
}

fn seed_media_with_ledger(store: &Store, library_root: &Path, title: &str) -> MediaId {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let path = library_root.join(format!("{title}.mkv"));
    std::fs::write(&path, b"x").unwrap();
    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: media.id,
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
    media.id
}

/// 没有 ledger 行的媒体不会出现在成员可见集合里（= hidden）。
fn seed_hidden_media(store: &Store, title: &str) -> MediaId {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    media.id
}

fn log_play(
    store: &Store,
    user_id: domain::UserId,
    media_id: MediaId,
    started_at: i64,
    device: &str,
) {
    store
        .upsert_session(&api::store::SessionRow {
            device_id: device.into(),
            user_id,
            media_id,
            season: None,
            episode: None,
            client: Some("test".into()),
            device_name: Some("test".into()),
            client_version: None,
            play_method: "local".into(),
            position_ms: 10,
            start_position_ms: 0,
            duration_ms: Some(100),
            paused: false,
            watched_ms: 10,
            rate_bps: 0,
            bytes_sent: 0,
            connections: 1,
            admin_ended: false,
            started_at,
            last_report_at: started_at,
            last_report_at_ms: started_at * 1000,
            ledger_id: None,
            play_session_id: None,
            last_event_sequence: None,
        })
        .unwrap();
    store
        .close_session(user_id, device, started_at + 1)
        .unwrap();
}

#[tokio::test]
async fn entries_skip_hidden_rows_and_hidden_count_covers_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let library_root = seed_visible_library(&store.lock(), tmp.path());
    let (member, token) = seed_member(&store.lock(), "alice");
    let (visible, hidden) = {
        let guard = store.lock();
        (
            seed_media_with_ledger(&guard, &library_root, "Visible"),
            seed_hidden_media(&guard, "Hidden"),
        )
    };
    {
        let guard = store.lock();
        log_play(&guard, member, visible, 3000, "dev-visible");
        log_play(&guard, member, hidden, 2000, "dev-hidden");
        log_play(&guard, member, visible, 1000, "dev-visible-2");
    }

    let data = history(&app, &token, "").await;
    let entries = data["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "hidden rows must not appear: {data}");
    assert_eq!(data["hidden_count"], 1, "scope hidden rows: {data}");
    assert_eq!(data["has_more"], false);
    assert_eq!(data["next_cursor"], Value::Null);
    assert_eq!(data["next_cursor_id"], Value::Null);
}

#[tokio::test]
async fn same_second_rows_page_without_gaps_or_duplicates() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let library_root = seed_visible_library(&store.lock(), tmp.path());
    let (member, token) = seed_member(&store.lock(), "bob");
    let visible = {
        let guard = store.lock();
        seed_media_with_ledger(&guard, &library_root, "SameSecond")
    };
    let same_second = 5_000_000;
    {
        let guard = store.lock();
        for device in ["dev-a", "dev-b", "dev-c"] {
            log_play(&guard, member, visible, same_second, device);
        }
    }

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<(String, String)> = None;
    for page in 0..3 {
        let query = match &cursor {
            Some((before, before_id)) => format!("limit=1&before={before}&before_id={before_id}"),
            None => "limit=1".to_string(),
        };
        let data = history(&app, &token, &query).await;
        let entries = data["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "page {page}: {data}");
        seen.push(entries[0]["id"].as_str().unwrap().to_string());
        if page < 2 {
            assert_eq!(data["has_more"], true, "page {page}: {data}");
            cursor = Some((
                data["next_cursor"].as_str().unwrap().to_string(),
                data["next_cursor_id"].as_str().unwrap().to_string(),
            ));
        } else {
            assert_eq!(data["has_more"], false, "page {page}: {data}");
            assert_eq!(data["next_cursor"], Value::Null);
        }
    }
    seen.sort();
    seen.dedup();
    assert_eq!(
        seen.len(),
        3,
        "same-second pagination must not repeat or drop rows"
    );
}

#[tokio::test]
async fn days_filter_applies_to_page_and_hidden_count() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let library_root = seed_visible_library(&store.lock(), tmp.path());
    let (member, token) = seed_member(&store.lock(), "carol");
    let (visible, hidden) = {
        let guard = store.lock();
        (
            seed_media_with_ledger(&guard, &library_root, "Recent"),
            seed_hidden_media(&guard, "HiddenRecent"),
        )
    };
    let now = now_secs();
    {
        let guard = store.lock();
        log_play(&guard, member, visible, now - 3600, "dev-recent");
        log_play(&guard, member, hidden, now - 7200, "dev-hidden-recent");
        log_play(&guard, member, visible, now - 10 * 86_400, "dev-old");
        log_play(&guard, member, hidden, now - 11 * 86_400, "dev-hidden-old");
    }

    let data = history(&app, &token, "days=5").await;
    let entries = data["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "days must filter the page: {data}");
    assert_eq!(
        data["hidden_count"], 1,
        "days must filter hidden_count: {data}"
    );
    assert_eq!(data["has_more"], false);
}

#[tokio::test]
async fn admin_scope_all_ignores_visibility() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let library_root = seed_visible_library(&store.lock(), tmp.path());
    let (member, _token) = seed_member(&store.lock(), "dave");
    let (visible, hidden) = {
        let guard = store.lock();
        (
            seed_media_with_ledger(&guard, &library_root, "AdminVisible"),
            seed_hidden_media(&guard, "AdminHidden"),
        )
    };
    {
        let guard = store.lock();
        log_play(&guard, member, visible, 2000, "dev-visible");
        log_play(&guard, member, hidden, 1000, "dev-hidden");
    }

    let data = history(&app, "management-secret", "scope=all").await;
    let entries = data["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "admin scope=all sees every row: {data}");
    assert_eq!(data["hidden_count"], 0);
}

#[tokio::test]
async fn member_without_visible_library_gets_empty_page() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let (member, token) = seed_member(&store.lock(), "erin");
    let hidden = seed_hidden_media(&store.lock(), "NoLibrary");
    log_play(&store.lock(), member, hidden, 1000, "dev-hidden");

    let data = history(&app, &token, "").await;
    assert_eq!(data["entries"].as_array().unwrap().len(), 0, "{data}");
    assert_eq!(data["has_more"], false);
    assert_eq!(
        data["hidden_count"], 1,
        "scope hidden rows are still counted"
    );
}

/// 真实回归：hidden_count 必须覆盖整个 scope，而不是旧的「前 20 万行」窗口。
///
/// fixture 用 raw SQL 批量写入 200_001 行（20 万 visible + 1 条最旧的 hidden），
/// 旧实现的 `list_logs(200_000, …)` 会漏掉排在窗口之外的那条 hidden。
#[tokio::test]
async fn hidden_count_counts_rows_beyond_the_legacy_cap() {
    const VISIBLE_ROWS: i64 = 200_000;

    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let library_root = seed_visible_library(&store.lock(), tmp.path());
    let (member, token) = seed_member(&store.lock(), "frank");
    let (visible, hidden) = {
        let guard = store.lock();
        (
            seed_media_with_ledger(&guard, &library_root, "CapVisible"),
            seed_hidden_media(&guard, "CapHidden"),
        )
    };

    let base = now_secs();
    let connection = rusqlite::Connection::open(tmp.path().join("data/subscribe.db")).unwrap();
    let transaction = connection.unchecked_transaction().unwrap();
    {
        let mut insert = transaction
            .prepare(
                "INSERT INTO playback_logs
                    (id, user_id, media_id, play_method, started_at, ended_at,
                     watched_ms, start_position_ms, end_position_ms, completed)
                 VALUES (?1, ?2, ?3, 'local', ?4, ?5, 0, 0, 0, 1)",
            )
            .unwrap();
        for index in 0..VISIBLE_ROWS {
            insert
                .execute(rusqlite::params![
                    format!("{index:032x}"),
                    member.to_string(),
                    visible.to_string(),
                    base - index,
                    base - index + 1
                ])
                .unwrap();
        }
        // 最旧的一行：超出旧实现 200_000 行窗口，且 media 对成员不可见。
        insert
            .execute(rusqlite::params![
                format!("{:032x}", 1_000_000u64),
                member.to_string(),
                hidden.to_string(),
                base - 20_000_000,
                base - 20_000_000 + 1
            ])
            .unwrap();
    }
    transaction.commit().unwrap();

    let data = history(&app, &token, "").await;
    assert_eq!(
        data["entries"].as_array().unwrap().len(),
        30,
        "page stays limit-sized: {data}"
    );
    assert_eq!(data["has_more"], true);
    assert_eq!(
        data["hidden_count"], 1,
        "hidden rows outside the legacy 200k window must still be counted: {data}"
    );
}
