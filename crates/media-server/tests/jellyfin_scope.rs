//! C1：未知 `ParentId` 不得退化成整库、user 路径必须校验、library 列表失败要 500。
//!
//! 全部使用假 provider，不访问真实数据/网络。

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, UserId};
use media_server::provider::{
    AuthenticateError, MediaItemMetadata, MediaItemSnapshot, MediaServerProvider, ServerLibrary,
    ServerUser,
};
use serde_json::Value;
use tower::ServiceExt;

const USER_A: &str = "00000000-0000-0000-0000-0000000000a1";
const USER_B: &str = "00000000-0000-0000-0000-0000000000b2";
const SERIES: &str = "00000000-0000-0000-0000-0000000000c3";
const MOVIE_ROOT: &str = "/media/movies";
const TV_ROOT: &str = "/media/tv";

fn user(raw: &str) -> UserId {
    UserId::from_str(raw).unwrap()
}

fn ledger(media_id: MediaId, path: &str) -> LedgerRow {
    LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: path.into(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    }
}

fn media(raw_id: &str, kind: MediaKind, title: &str) -> Media {
    Media {
        id: MediaId::from_str(raw_id).unwrap(),
        kind,
        title: title.into(),
        year: Some(2020),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn movie(title: &str, path: &str, genres: &[&str], studios: &[&str]) -> MediaItemSnapshot {
    let media = media(&MediaId::new().to_string(), MediaKind::Movie, title);
    MediaItemSnapshot {
        row: ledger(media.id, path),
        media,
        ticks: 0,
        play_count: 0,
        played: false,
        unplayed_item_count: None,
        parent_id: "lib-movies".into(),
        is_series: false,
        chapters: Vec::new(),
        intro_start_ms: None,
        intro_end_ms: None,
        outro_start_ms: None,
        outro_end_ms: None,
        metadata: MediaItemMetadata {
            genres: genres.iter().map(|g| (*g).to_string()).collect(),
            studios: studios.iter().map(|s| (*s).to_string()).collect(),
            ..Default::default()
        },
    }
}

fn series(title: &str, path: &str) -> MediaItemSnapshot {
    let media = media(SERIES, MediaKind::Tv, title);
    MediaItemSnapshot {
        row: ledger(media.id, path),
        media,
        ticks: 0,
        play_count: 0,
        played: false,
        unplayed_item_count: None,
        parent_id: "lib-tv".into(),
        is_series: true,
        chapters: Vec::new(),
        intro_start_ms: None,
        intro_end_ms: None,
        outro_start_ms: None,
        outro_end_ms: None,
        metadata: MediaItemMetadata::default(),
    }
}

struct FakeProvider {
    libraries_fail: bool,
    items: Vec<MediaItemSnapshot>,
}

impl FakeProvider {
    fn new(libraries_fail: bool) -> Self {
        Self {
            libraries_fail,
            items: vec![
                movie("Alpha", "/media/movies/Alpha.mkv", &["Drama"], &["Netflix"]),
                series("Beta", "/media/tv/Beta"),
            ],
        }
    }
}

#[async_trait::async_trait]
impl MediaServerProvider for FakeProvider {
    fn server_id(&self) -> String {
        "scope-server".into()
    }

    async fn authenticate_password(
        &self,
        _username: &str,
        _password: &str,
    ) -> Result<Option<(String, ServerUser)>, AuthenticateError> {
        Ok(None)
    }

    async fn user_id_by_token(&self, token: &str) -> Result<Option<UserId>, String> {
        Ok(match token {
            "token-a" => Some(user(USER_A)),
            "token-b" => Some(user(USER_B)),
            _ => None,
        })
    }

    async fn get_user(&self, user_id: UserId) -> Result<Option<ServerUser>, String> {
        Ok(match user_id {
            id if id == user(USER_A) => Some(ServerUser {
                id,
                login: "alice".into(),
                is_admin: false,
            }),
            id if id == user(USER_B) => Some(ServerUser {
                id,
                login: "bob".into(),
                is_admin: false,
            }),
            _ => None,
        })
    }

    async fn list_visible_libraries(&self, _user_id: UserId) -> Result<Vec<ServerLibrary>, String> {
        if self.libraries_fail {
            return Err("library backend down".into());
        }
        Ok(vec![
            ServerLibrary {
                id: "lib-movies".into(),
                name: "Movies".into(),
                kind: MediaKind::Movie,
                exclude_from_home: false,
                root_paths: vec![PathBuf::from(MOVIE_ROOT)],
                cover_path: None,
                cover_tag: String::new(),
            },
            ServerLibrary {
                id: "lib-tv".into(),
                name: "TV".into(),
                kind: MediaKind::Tv,
                exclude_from_home: false,
                root_paths: vec![PathBuf::from(TV_ROOT)],
                cover_path: None,
                cover_tag: String::new(),
            },
        ])
    }

    async fn list_visible_items(
        &self,
        _user_id: UserId,
        parent_id: Option<&str>,
    ) -> Result<Vec<MediaItemSnapshot>, String> {
        Ok(match parent_id {
            None => self.items.clone(),
            Some("lib-movies") => self
                .items
                .iter()
                .filter(|item| !item.is_series)
                .cloned()
                .collect(),
            Some(id) if id.replace('-', "") == SERIES.replace('-', "") => self
                .items
                .iter()
                .filter(|item| item.is_series)
                .cloned()
                .collect(),
            Some(_) => Vec::new(),
        })
    }

    async fn resolve_single_item(
        &self,
        _user_id: UserId,
        id: &str,
    ) -> Result<Option<MediaItemSnapshot>, String> {
        Ok(self.items.iter().find(|item| {
            item.is_series && item.media.id.to_string().replace('-', "") == id.replace('-', "")
        }).cloned())
    }

    async fn resolve_cover_bytes(
        &self,
        _user_id: Option<UserId>,
        _id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }

    async fn resolve_poster_bytes(
        &self,
        _user_id: Option<UserId>,
        _id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }

    async fn resolve_stream_source(
        &self,
        _user_id: UserId,
        _id: &str,
    ) -> Result<Option<(PathBuf, bool)>, String> {
        Ok(None)
    }

    async fn set_user_marks(
        &self,
        _user_id: UserId,
        _id: &str,
        _played: Option<bool>,
        _favorite: Option<bool>,
    ) -> Result<bool, String> {
        Ok(true)
    }

    async fn update_progress(
        &self,
        _user_id: UserId,
        _id: &str,
        _position_ms: i64,
        _paused: bool,
    ) -> Result<(), String> {
        Ok(())
    }
}

fn app(libraries_fail: bool) -> axum::Router {
    media_server::routes(Arc::new(FakeProvider::new(libraries_fail)))
}

async fn get(app: &axum::Router, uri: &str, token: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

fn folder_id(root: &str) -> String {
    format!(
        "folder_{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, root.as_bytes()).simple()
    )
}

fn genre_id(name: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes())
        .hyphenated()
        .to_string()
}

fn item_count(body: &Value) -> usize {
    body["Items"].as_array().map(Vec::len).unwrap_or(0)
}

#[tokio::test]
async fn unknown_parent_returns_an_empty_page_instead_of_the_whole_library() {
    let (status, body) = get(&app(false), "/Items?ParentId=does-not-exist", "token-a").await;
    assert_eq!(status, StatusCode::OK, "未知 ParentId 不能变成 404：{body}");
    assert_eq!(body["Items"].as_array().map(Vec::len), Some(0), "body={body}");
    assert_eq!(body["TotalRecordCount"], 0, "body={body}");
}

#[tokio::test]
async fn missing_parent_still_returns_every_visible_item() {
    let (status, body) = get(&app(false), "/Items", "token-a").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(item_count(&body), 2, "Root 行为不变：{body}");
    assert_eq!(body["TotalRecordCount"], 2);
}

#[tokio::test]
async fn library_parent_keeps_its_items() {
    let (status, body) = get(&app(false), "/Items?ParentId=lib-movies", "token-a").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(item_count(&body), 1, "body={body}");
    assert_eq!(body["Items"][0]["Name"], "Alpha");
}

#[tokio::test]
async fn series_parent_keeps_its_episodes() {
    let (status, body) = get(&app(false), &format!("/Items?ParentId={SERIES}"), "token-a").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(item_count(&body), 1, "body={body}");
    assert_eq!(body["Items"][0]["Name"], "Beta");
}

#[tokio::test]
async fn folder_parent_filters_by_root_prefix() {
    let uri = format!("/Items?ParentId={}", folder_id(MOVIE_ROOT));
    let (status, body) = get(&app(false), &uri, "token-a").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(item_count(&body), 1, "body={body}");
    assert_eq!(body["Items"][0]["Name"], "Alpha");
}

#[tokio::test]
async fn genre_parent_filters_by_genre() {
    let uri = format!("/Items?ParentId={}", genre_id("Drama"));
    let (status, body) = get(&app(false), &uri, "token-a").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(item_count(&body), 1, "body={body}");
    assert_eq!(body["Items"][0]["Name"], "Alpha");
}

#[tokio::test]
async fn studio_parent_filters_by_studio() {
    let uri = format!("/Items?ParentId={}", genre_id("Netflix"));
    let (status, body) = get(&app(false), &uri, "token-a").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(item_count(&body), 1, "body={body}");
    assert_eq!(body["Items"][0]["Name"], "Alpha");
}

#[tokio::test]
async fn user_profile_accepts_self_and_rejects_other_users() {
    let app = app(false);
    let compact = USER_A.replace('-', "");

    for (uri, expected) in [
        ("/Users/me".to_string(), 200),
        ("/Users/Me".to_string(), 200),
        (format!("/Users/{USER_A}"), 200),
        (format!("/Users/{compact}"), 200),
        ("/Users/alice".to_string(), 200),
    ] {
        let (status, body) = get(&app, &uri, "token-a").await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        assert_eq!(body["Id"], USER_A, "{uri}: 必须返回本人");
        assert_eq!(body["Name"], "alice", "{uri}");
        assert_eq!(expected, 200);
    }

    for uri in [
        format!("/Users/{USER_B}"),
        "/Users/bob".to_string(),
    ] {
        let (status, _) = get(&app, &uri, "token-a").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri} 必须 404");
    }
}

#[tokio::test]
async fn user_scoped_routes_reject_other_users() {
    let app = app(false);
    let other = USER_B;
    for uri in [
        format!("/Users/{other}/Items"),
        format!("/Users/{other}/Items/Counts"),
        format!("/Users/{other}/Items/Resume"),
        format!("/Users/{other}/Items/Latest"),
        format!("/Users/{other}/Views"),
    ] {
        let (status, _) = get(&app, &uri, "token-a").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri} 必须 404");
    }

    // 本人路径仍然可用。
    for uri in [
        format!("/Users/{USER_A}/Items"),
        format!("/Users/{USER_A}/Items/Counts"),
        format!("/Users/{USER_A}/Items/Resume"),
        format!("/Users/{USER_A}/Items/Latest"),
        format!("/Users/{USER_A}/Views"),
        "/Users/Me/Items/Latest".to_string(),
    ] {
        let (status, body) = get(&app, &uri, "token-a").await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    }
}

#[tokio::test]
async fn library_list_failure_is_a_server_error() {
    let app = app(true);
    for uri in ["/Items", "/UserViews", "/Library/MediaFolders", "/Genres"] {
        let (status, body) = get(&app, uri, "token-a").await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{uri} 必须 500 而不是空 200：{body}"
        );
    }
}
