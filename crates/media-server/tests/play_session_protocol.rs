use std::sync::Arc;

use axum::http::StatusCode;
use domain::UserId;
use media_server::provider::{
    AuthenticateError, MediaItemSnapshot, MediaServerProvider, PlaybackClientInfo, PlaybackEvent,
    PlaybackEventIdentity, ServerLibrary, ServerUser,
};
use media_server::routes;
use serde_json::json;
use tower::ServiceExt;

#[derive(Default)]
struct TestProvider;

#[async_trait::async_trait]
impl MediaServerProvider for TestProvider {
    fn server_id(&self) -> String {
        "test-server".to_string()
    }
    async fn authenticate_password(
        &self,
        _username: &str,
        _password: &str,
    ) -> Result<Option<(String, ServerUser)>, AuthenticateError> {
        Ok(None)
    }
    async fn user_id_by_token(&self, _token: &str) -> Result<Option<UserId>, String> {
        Ok(Some(UserId::new()))
    }
    async fn get_user(&self, id: UserId) -> Result<Option<ServerUser>, String> {
        Ok(Some(ServerUser {
            id,
            login: "admin".into(),
            is_admin: true,
        }))
    }
    async fn list_visible_libraries(&self, _user_id: UserId) -> Result<Vec<ServerLibrary>, String> {
        Ok(Vec::new())
    }
    async fn list_visible_items(
        &self,
        _user_id: UserId,
        _parent_id: Option<&str>,
    ) -> Result<Vec<MediaItemSnapshot>, String> {
        Ok(Vec::new())
    }
    async fn resolve_single_item(
        &self,
        _user_id: UserId,
        _id: &str,
    ) -> Result<Option<MediaItemSnapshot>, String> {
        Ok(None)
    }
    async fn resolve_cover_bytes(&self, _user_id: Option<UserId>, _id: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }
    async fn resolve_poster_bytes(&self, _user_id: Option<UserId>, _id: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }
    async fn resolve_stream_source(&self, _user: UserId, _id: &str) -> Result<Option<(std::path::PathBuf, bool)>, String> {
        Ok(Some((std::path::PathBuf::from("/movies/test.mkv"), false)))
    }
    async fn set_user_marks(&self, _user: UserId, _id: &str, _played: Option<bool>, _fav: Option<bool>) -> Result<bool, String> {
        Ok(true)
    }
    async fn update_progress(&self, _user: UserId, _id: &str, _pos: i64, _paused: bool) -> Result<(), String> {
        Ok(())
    }
    async fn report_playback_event(
        &self,
        _user: UserId,
        _id: &str,
        _pos: i64,
        _paused: bool,
        _client: PlaybackClientInfo,
        _event: PlaybackEvent,
        _identity: PlaybackEventIdentity,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[tokio::test]
async fn sequence_and_id_length_validation() {
    let provider = Arc::new(TestProvider::default());
    let app = routes::routes(provider);

    // Negative sequence returns 400
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/Sessions/Playing/Progress")
        .header("Authorization", "MediaBrowser Token=\"test\"")
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&json!({
            "ItemId": "item1",
            "PositionTicks": 100000,
            "EventSequence": -1,
        })).unwrap()))
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Too long PlaySessionId returns 400
    let long_id = "a".repeat(129);
    let req2 = axum::http::Request::builder()
        .method("POST")
        .uri("/Sessions/Playing/Progress")
        .header("Authorization", "MediaBrowser Token=\"test\"")
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&json!({
            "ItemId": "item1",
            "PositionTicks": 100000,
            "PlaySessionId": long_id,
        })).unwrap()))
        .unwrap();

    let resp2 = app.oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::BAD_REQUEST);
}
