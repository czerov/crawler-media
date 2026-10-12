//! DB1: 设备撤销检查的日志脱敏**不得**改变鉴权行为。
//!
//! `check_device_revocation` 现在用 `log_device_id` 只给日志脱敏；撤销检查仍必须
//! 收到真实 device id（包括「没有 DeviceId 时回退到会话 token」的兜底值）。
//! 这里通过公开的 `media_routes` seam 记录 provider 实际收到的 device id 来断言，
//! 不安装全局 tracing subscriber（并行测试下会竞态）。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::UserId;
use media_server::provider::{
    AuthenticateError, MediaItemSnapshot, MediaServerProvider, ServerLibrary, ServerUser,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const TOKEN: &str = "play-token";

struct RecordingProvider {
    user_id: UserId,
    revoked: Vec<&'static str>,
    seen_device_ids: Mutex<Vec<String>>,
}

impl RecordingProvider {
    fn new(revoked: Vec<&'static str>) -> Self {
        Self {
            user_id: UserId::new(),
            revoked,
            seen_device_ids: Mutex::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<String> {
        self.seen_device_ids.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl MediaServerProvider for RecordingProvider {
    fn server_id(&self) -> String {
        "test-server".into()
    }

    async fn authenticate_password(
        &self,
        _username: &str,
        _password: &str,
    ) -> Result<Option<(String, ServerUser)>, AuthenticateError> {
        Ok(None)
    }

    async fn user_id_by_token(&self, token: &str) -> Result<Option<UserId>, String> {
        Ok((token == TOKEN).then_some(self.user_id))
    }

    async fn get_user(&self, user_id: UserId) -> Result<Option<ServerUser>, String> {
        Ok((user_id == self.user_id).then(|| ServerUser {
            id: self.user_id,
            login: "viewer".into(),
            is_admin: false,
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

    async fn is_device_revoked(&self, _user_id: UserId, device_id: &str) -> Result<bool, String> {
        self.seen_device_ids
            .lock()
            .unwrap()
            .push(device_id.to_string());
        Ok(self.revoked.iter().any(|revoked| *revoked == device_id))
    }
}

fn stream_request(device_id: Option<&str>) -> Request<Body> {
    let mut builder = Request::get("/Videos/episode/stream")
        .header("authorization", format!("Bearer {TOKEN}"));
    if let Some(device_id) = device_id {
        builder = builder.header("X-Emby-Device-Id", device_id);
    }
    builder.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn fallback_revocation_check_uses_the_real_token_not_the_log_placeholder() {
    // 没有 DeviceId：撤销检查回退到会话 token，日志里换成占位符，但检查必须用真值。
    let provider = Arc::new(RecordingProvider::new(vec![TOKEN]));
    let response = media_server::media_routes(provider.clone())
        .oneshot(stream_request(None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let seen = provider.seen();
    assert_eq!(seen, vec![TOKEN.to_string()], "撤销检查必须收到真实 token");
    assert!(
        !seen.iter().any(|id| id.contains("session-token")),
        "脱敏占位符绝不能进入鉴权路径: {seen:?}"
    );
}

#[tokio::test]
async fn header_device_id_is_used_for_the_revocation_check() {
    let provider = Arc::new(RecordingProvider::new(vec!["banned-device"]));
    let response = media_server::media_routes(provider.clone())
        .oneshot(stream_request(Some("banned-device")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(provider.seen(), vec!["banned-device".to_string()]);
}

#[tokio::test]
async fn non_revoked_device_still_checks_the_session_token() {
    // device id 与 token 不同且都未被撤销：先查 device id，再查 token，两次都用真值。
    let provider = Arc::new(RecordingProvider::new(Vec::new()));
    let response = media_server::media_routes(provider.clone())
        .oneshot(stream_request(Some("phone-1")))
        .await
        .unwrap();

    assert_ne!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        provider.seen(),
        vec!["phone-1".to_string(), TOKEN.to_string()]
    );
}
