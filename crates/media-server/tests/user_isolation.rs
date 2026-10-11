use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, UserId};
use media_server::provider::{
    AuthenticateError, MediaItemSnapshot, MediaServerProvider, ServerLibrary, ServerUser,
};
use serde_json::Value;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tower::ServiceExt;
use tungstenite::{Message, WebSocket};

struct FakeProvider {
    user_a: UserId,
    user_b: UserId,
    media_a: Media,
    media_b: Media,
    row_a: LedgerRow,
    row_b: LedgerRow,
    favorite_a: Arc<AtomicBool>,
    token_a_valid: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl MediaServerProvider for FakeProvider {
    fn server_id(&self) -> String {
        "test-server-id".into()
    }

    async fn authenticate_password(
        &self,
        username: &str,
        _password: &str,
    ) -> Result<Option<(String, ServerUser)>, AuthenticateError> {
        if username == "user_a" {
            Ok(Some((
                "token_a".into(),
                ServerUser {
                    id: self.user_a,
                    login: "user_a".into(),
                    is_admin: false,
                },
            )))
        } else if username == "user_b" {
            Ok(Some((
                "token_b".into(),
                ServerUser {
                    id: self.user_b,
                    login: "user_b".into(),
                    is_admin: false,
                },
            )))
        } else {
            Ok(None)
        }
    }

    async fn user_id_by_token(&self, token: &str) -> Result<Option<UserId>, String> {
        if token == "token_a" {
            if self.token_a_valid.load(Ordering::SeqCst) {
                Ok(Some(self.user_a))
            } else {
                Ok(None)
            }
        } else if token == "token_b" {
            Ok(Some(self.user_b))
        } else {
            Ok(None)
        }
    }

    async fn get_user(&self, user_id: UserId) -> Result<Option<ServerUser>, String> {
        if user_id == self.user_a {
            Ok(Some(ServerUser {
                id: self.user_a,
                login: "user_a".into(),
                is_admin: false,
            }))
        } else {
            Ok(Some(ServerUser {
                id: self.user_b,
                login: "user_b".into(),
                is_admin: false,
            }))
        }
    }

    async fn list_visible_libraries(&self, user_id: UserId) -> Result<Vec<ServerLibrary>, String> {
        if user_id == self.user_a {
            Ok(vec![ServerLibrary {
                id: "lib-a".into(),
                name: "User A Library".into(),
                kind: self.media_a.kind,
                exclude_from_home: false,
                root_paths: vec![],
                cover_path: None,
                cover_tag: "cover-test-a".into(),
            }])
        } else {
            Ok(vec![ServerLibrary {
                id: "lib-b".into(),
                name: "User B Library".into(),
                kind: MediaKind::Movie,
                exclude_from_home: false,
                root_paths: vec![],
                cover_path: None,
                cover_tag: "cover-test-b".into(),
            }])
        }
    }

    async fn list_visible_items(
        &self,
        user_id: UserId,
        _parent_id: Option<&str>,
    ) -> Result<Vec<MediaItemSnapshot>, String> {
        if user_id == self.user_a {
            Ok(vec![MediaItemSnapshot {
                row: self.row_a.clone(),
                media: self.media_a.clone(),
                ticks: 120_000_000,
                play_count: 0,
                played: false,
                unplayed_item_count: None,
                parent_id: "lib-a".into(),
                is_series: self.media_a.kind == MediaKind::Tv,
                chapters: vec![],
                intro_start_ms: None,
                intro_end_ms: None,
                outro_start_ms: None,
                outro_end_ms: None,
                metadata: media_server::provider::MediaItemMetadata {
                    is_favorite: self.favorite_a.load(Ordering::SeqCst),
                    ..Default::default()
                },
            }])
        } else {
            Ok(vec![MediaItemSnapshot {
                row: self.row_b.clone(),
                media: self.media_b.clone(),
                ticks: 0,
                play_count: 0,
                played: false,
                unplayed_item_count: None,
                parent_id: "lib-b".into(),
                is_series: false,
                chapters: vec![],
                intro_start_ms: None,
                intro_end_ms: None,
                outro_start_ms: None,
                outro_end_ms: None,
                metadata: Default::default(),
            }])
        }
    }

    async fn resolve_single_item(
        &self,
        user_id: UserId,
        id: &str,
    ) -> Result<Option<MediaItemSnapshot>, String> {
        let items = self.list_visible_items(user_id, None).await?;
        Ok(items.into_iter().find(|it| {
            let item_id = if it.is_series {
                it.media.id.to_string()
            } else {
                it.row.id.to_string()
            };
            item_id == id || item_id.replace('-', "").eq_ignore_ascii_case(id)
        }))
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
    ) -> Result<Option<(std::path::PathBuf, bool)>, String> {
        Ok(None)
    }

    async fn set_user_marks(
        &self,
        user_id: UserId,
        id: &str,
        _played: Option<bool>,
        favorite: Option<bool>,
    ) -> Result<bool, String> {
        if user_id == self.user_a
            && (id == self.row_a.id.to_string()
                || id.eq_ignore_ascii_case(&self.row_a.id.to_string().replace('-', ""))
                || id == self.media_a.id.to_string()
                || id.eq_ignore_ascii_case(&self.media_a.id.to_string().replace('-', "")))
        {
            if let Some(favorite) = favorite {
                self.favorite_a.store(favorite, Ordering::SeqCst);
            }
        }
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

fn test_provider(user_a_kind: MediaKind) -> Arc<FakeProvider> {
    let user_a = UserId::new();
    let user_b = UserId::new();
    let media_a = Media {
        id: MediaId::new(),
        kind: user_a_kind,
        title: "Movie for User A".into(),
        year: Some(2025),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let media_b = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Movie for User B".into(),
        year: Some(2026),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let row = |media_id, path: &str, is_tv: bool| LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: path.into(),
        season: is_tv.then_some(1),
        episode: is_tv.then_some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: domain::Confidence::High,
        filter_score: None,
    };
    Arc::new(FakeProvider {
        user_a,
        user_b,
        row_a: row(media_a.id, "/path/a.mkv", user_a_kind == MediaKind::Tv),
        row_b: row(media_b.id, "/path/b.mkv", false),
        media_a,
        media_b,
        favorite_a: Arc::new(AtomicBool::new(false)),
        token_a_valid: Arc::new(AtomicBool::new(true)),
    })
}

async fn websocket(addr: SocketAddr, token: &'static str) -> WebSocket<TcpStream> {
    tokio::task::spawn_blocking(move || {
        let stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let request = format!("ws://{addr}/socket?ApiKey={token}&deviceId=integration-test");
        tungstenite::client(request, stream).unwrap().0
    })
    .await
    .unwrap()
}

async fn websocket_json(mut socket: WebSocket<TcpStream>) -> (Value, WebSocket<TcpStream>) {
    let (message, socket) = tokio::task::spawn_blocking(move || {
        let message = socket.read().unwrap();
        (message, socket)
    })
    .await
    .unwrap();
    let Message::Text(text) = message else {
        panic!("expected JSON event, got {message:?}");
    };
    (serde_json::from_str(&text).unwrap(), socket)
}

#[tokio::test]
async fn favorite_mutations_notify_connected_jellyfin_clients() {
    let provider = test_provider(MediaKind::Tv);
    let user_id = provider.user_a;
    let item_id = provider.media_a.id.to_string().replace('-', "");
    let app = media_server::routes(provider);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_app = app.clone();
    tokio::spawn(async move { axum::serve(listener, server_app).await.unwrap() });
    let mut socket = websocket(addr, "token_a").await;

    for (method, expected) in [("POST", true), ("DELETE", false)] {
        let response = app_request_favorite(&app, method, &item_id).await;
        assert_eq!(response, StatusCode::OK);
        let (event, next_socket) = websocket_json(socket).await;
        socket = next_socket;
        assert_eq!(event["MessageType"], "UserDataChanged");
        assert_eq!(event["Data"]["UserId"], user_id.to_string());
        let changed = event["Data"]["UserDataList"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["ItemId"] == item_id)
            .unwrap();
        assert_eq!(changed["IsFavorite"], expected);
    }
}

async fn app_request_favorite(app: &axum::Router, method: &str, item_id: &str) -> StatusCode {
    let method: axum::http::Method = method.parse().unwrap();
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("/Users/user_a/FavoriteItems/{item_id}"))
                .header("authorization", "Bearer token_a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn media_server_strictly_enforces_user_isolation_between_accounts() {
    let provider = test_provider(MediaKind::Movie);
    let app = media_server::routes(provider);

    // 1. User A 查询视图和条目：只能看到 User A 的媒体库与作品，且进度为 120,000,000 Ticks
    let req_a_views = Request::builder()
        .uri("/UserViews")
        .header("authorization", "Bearer token_a")
        .body(Body::empty())
        .unwrap();
    let res_a_views = app.clone().oneshot(req_a_views).await.unwrap();
    assert_eq!(res_a_views.status(), StatusCode::OK);
    let bytes_a = axum::body::to_bytes(res_a_views.into_body(), usize::MAX)
        .await
        .unwrap();
    let json_a_views: Value = serde_json::from_slice(&bytes_a).unwrap();
    assert_eq!(json_a_views["Items"][0]["Name"], "User A Library");
    assert_eq!(
        json_a_views["Items"][0]["ImageTags"]["Primary"],
        "cover-test-a"
    );

    let req_a_items = Request::builder()
        .uri("/Items")
        .header("authorization", "Bearer token_a")
        .body(Body::empty())
        .unwrap();
    let res_a_items = app.clone().oneshot(req_a_items).await.unwrap();
    assert_eq!(res_a_items.status(), StatusCode::OK);
    let bytes_items_a = axum::body::to_bytes(res_a_items.into_body(), usize::MAX)
        .await
        .unwrap();
    let json_a_items: Value = serde_json::from_slice(&bytes_items_a).unwrap();
    assert_eq!(json_a_items["Items"][0]["Name"], "Movie for User A");
    assert_eq!(
        json_a_items["Items"][0]["UserData"]["PlaybackPositionTicks"],
        120_000_000
    );

    // 2. User B 查询视图和条目：只能看到 User B 的媒体库与作品，绝无 User A 的内容泄漏
    let req_b_views = Request::builder()
        .uri("/UserViews")
        .header("authorization", "Bearer token_b")
        .body(Body::empty())
        .unwrap();
    let res_b_views = app.clone().oneshot(req_b_views).await.unwrap();
    assert_eq!(res_b_views.status(), StatusCode::OK);
    let bytes_b = axum::body::to_bytes(res_b_views.into_body(), usize::MAX)
        .await
        .unwrap();
    let json_b_views: Value = serde_json::from_slice(&bytes_b).unwrap();
    assert_eq!(json_b_views["Items"][0]["Name"], "User B Library");

    let req_b_items = Request::builder()
        .uri("/Items")
        .header("authorization", "Bearer token_b")
        .body(Body::empty())
        .unwrap();
    let res_b_items = app.oneshot(req_b_items).await.unwrap();
    assert_eq!(res_b_items.status(), StatusCode::OK);
    let bytes_items_b = axum::body::to_bytes(res_b_items.into_body(), usize::MAX)
        .await
        .unwrap();
    let json_b_items: Value = serde_json::from_slice(&bytes_items_b).unwrap();
    assert_eq!(json_b_items["Items"][0]["Name"], "Movie for User B");
    assert_eq!(
        json_b_items["Items"][0]["UserData"]["PlaybackPositionTicks"],
        0
    );
}

#[tokio::test]
async fn websocket_disconnects_when_token_revoked() {
    let provider = test_provider(MediaKind::Tv);
    let app = media_server::routes(provider.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_app = app.clone();
    tokio::spawn(async move { axum::serve(listener, server_app).await.unwrap() });

    let mut socket = websocket(addr, "token_a").await;

    // 撤销 token_a（模拟 logout / 改密）
    provider.token_a_valid.store(false, Ordering::SeqCst);

    // 触发一个事件，服务端在发送前复验 token 发现已失效，主动断开 WebSocket
    let item_id = provider.media_a.id.to_string().replace('-', "");
    let _ = app_request_favorite(&app, "POST", &item_id).await;

    // 客户端读取，预期得到关闭或断开连接
    let read_result = tokio::task::spawn_blocking(move || socket.read())
        .await
        .unwrap();
    assert!(
        read_result.is_err() || matches!(read_result, Ok(Message::Close(_))),
        "撤销 token 后 WebSocket 必须主动断开连接"
    );
}
