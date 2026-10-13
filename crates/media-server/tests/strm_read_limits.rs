use std::path::PathBuf;
use std::sync::Arc;

use axum::http::StatusCode;
use domain::UserId;
use media_server::provider::{
    AuthenticateError, MediaItemSnapshot, MediaServerProvider, ServerLibrary, ServerUser,
};
use media_server::routes;
use tower::ServiceExt;

#[derive(Default)]
struct TestStrmProvider {
    strm_path: PathBuf,
}

#[async_trait::async_trait]
impl MediaServerProvider for TestStrmProvider {
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
        Ok(Some((self.strm_path.clone(), true)))
    }
    async fn set_user_marks(&self, _user: UserId, _id: &str, _played: Option<bool>, _fav: Option<bool>) -> Result<bool, String> {
        Ok(true)
    }
    async fn update_progress(&self, _user: UserId, _id: &str, _pos: i64, _paused: bool) -> Result<(), String> {
        Ok(())
    }
}

#[tokio::test]
async fn strm_file_over_64kib_returns_bad_gateway() {
    let tmp = tempfile::tempdir().unwrap();
    let strm_path = tmp.path().join("large.strm");

    // Write 65537 bytes (> 64KiB = 65536)
    let large_bytes = vec![b'a'; 65_537];
    std::fs::write(&strm_path, large_bytes).unwrap();

    let provider = Arc::new(TestStrmProvider { strm_path });
    let app = routes::media_routes(provider);

    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/Videos/item1/stream")
        .header("Authorization", "MediaBrowser Token=\"test\"")
        .body(axum::body::Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}
