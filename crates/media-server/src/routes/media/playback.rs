use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};
use std::path::Path as FsPath;
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::auth::{AuthUser, authenticate};
use crate::dto;
use crate::provider::{MediaServerProvider, SubtitleDeliveryError};

use super::super::AppState;

pub(super) fn routes(provider: Arc<dyn MediaServerProvider>) -> Router {
    let stream_provider = provider.clone();
    Router::new()
        .route("/Videos/{id}/stream", get(stream))
        .route(
            "/Videos/{id}/Subtitles/{index}/Stream.{codec}",
            get(subtitle_stream),
        )
        .route_layer(axum::middleware::from_fn(move |req, next| {
            let provider = stream_provider.clone();
            authenticate(provider, req, next)
        }))
        .with_state(AppState::new(provider))
}

/// 会话 token 的日志占位：日志里永不出现 token 原文。
const SESSION_TOKEN_PLACEHOLDER: &str = "<session-token>";

/// 撤销检查用的真实 device id：user 字段优先，其次请求头，最后回退会话 token。
/// 鉴权必须使用真实值，只有日志走 [`log_device_id`]。
fn resolve_device_id<'a>(
    user_device_id: Option<&'a str>,
    header_device_id: Option<&'a str>,
    token: &'a str,
) -> &'a str {
    user_device_id.or(header_device_id).unwrap_or(token)
}

/// 日志用的 device id：解析结果就是会话 token 时返回固定占位，避免
/// 「没有 DeviceId 时回退到 token」的兜底值把会话凭证写进日志。
fn log_device_id(
    user_device_id: Option<&str>,
    header_device_id: Option<&str>,
    token: &str,
) -> String {
    let device_id = resolve_device_id(user_device_id, header_device_id, token);
    if device_id == token {
        SESSION_TOKEN_PLACEHOLDER.to_string()
    } else {
        device_id.to_string()
    }
}

async fn check_device_revocation(
    provider: &dyn MediaServerProvider,
    user: &AuthUser,
    headers: &HeaderMap,
    action: &str,
) -> Result<(), StatusCode> {
    let header_device_id = headers
        .get("X-Emby-Device-Id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let user_device_id = user.device_id.as_deref();
    // 鉴权用真实值；日志用脱敏值（两者只有「兜底到 token」这一种情况不同）。
    let device_id = resolve_device_id(user_device_id, header_device_id, &user.token);
    let device_id_for_log = log_device_id(user_device_id, header_device_id, &user.token);

    match provider.is_device_revoked(user.id, device_id).await {
        Ok(true) => {
            tracing::warn!(
                user_id = %user.id,
                device_id = %device_id_for_log,
                "{action}请求被阻断：设备已撤销"
            );
            return Err(StatusCode::FORBIDDEN);
        }
        Ok(false) => {}
        Err(err) => {
            tracing::error!(
                user_id = %user.id,
                device_id = %device_id_for_log,
                error = %err,
                "检查设备撤销状态失败"
            );
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    if device_id != user.token.as_str() {
        match provider.is_device_revoked(user.id, &user.token).await {
            Ok(true) => {
                tracing::warn!(user_id = %user.id, "{action}请求被阻断：会话Token关联设备已撤销");
                return Err(StatusCode::FORBIDDEN);
            }
            Ok(false) => {}
            Err(err) => {
                tracing::error!(user_id = %user.id, error = %err, "检查Token撤销状态失败");
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
    }
    Ok(())
}

async fn subtitle_stream(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path((id, index, codec)): Path<(String, u32, String)>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    check_device_revocation(state.provider.as_ref(), &user, &headers, "Jellyfin 字幕").await?;

    let wants_vtt = codec.eq_ignore_ascii_case("vtt");
    // 协议层只调用 provider seam；同步提取与 single-flight 都在 provider 内部。
    match state
        .provider
        .deliver_subtitle(user.id, &id, index, wants_vtt)
        .await
    {
        Ok(Some(payload)) => Ok((
            [(axum::http::header::CONTENT_TYPE, payload.content_type)],
            payload.bytes,
        )
            .into_response()),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(SubtitleDeliveryError::Busy) => Err(StatusCode::SERVICE_UNAVAILABLE),
        Err(SubtitleDeliveryError::Timeout) => Err(StatusCode::GATEWAY_TIMEOUT),
        Err(SubtitleDeliveryError::Cancelled) => Err(StatusCode::CONFLICT),
        Err(SubtitleDeliveryError::Failed(message)) => {
            tracing::error!(item_id = %id, index, error = %message, "Jellyfin 交付字幕失败");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
        Err(SubtitleDeliveryError::Unsupported) => {
            tracing::error!(item_id = %id, index, "Jellyfin provider 未实现字幕交付");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn stream(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    check_device_revocation(state.provider.as_ref(), &user, &headers, "Jellyfin 取流").await?;

    let (path, is_strm) = state
        .provider
        .resolve_stream_source(user.id, &id)
        .await
        .map_err(|error| {
            tracing::error!(%error, user_id = %user.id, item_id = %id, "failed to resolve Jellyfin stream source");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    if is_strm {
        return match library::read_strm_url(&path) {
            Some(direct_url) => {
                Ok((StatusCode::FOUND, [(header::LOCATION, direct_url)]).into_response())
            }
            None => {
                tracing::warn!(user_id = %user.id, item_id = %id, "Jellyfin STRM file has no usable URL");
                Err(StatusCode::NOT_FOUND)
            }
        };
    }

    let file = File::open(&path).await.map_err(|_| StatusCode::NOT_FOUND)?;
    stream_file(file, &path, headers).await
}

async fn stream_file(
    mut file: File,
    path: &FsPath,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let content_type = video_content_type(&path);
    let file_size = file
        .metadata()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .len();
    let range_header = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok());
    if let Some(header_value) = range_header {
        let Some(spec) = playback::parse_range(header_value, file_size) else {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [
                    (header::CONTENT_RANGE, format!("bytes */{file_size}")),
                    (header::ACCEPT_RANGES, "bytes".into()),
                ],
                Body::empty(),
            )
                .into_response());
        };
        file.seek(tokio::io::SeekFrom::Start(spec.start))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let stream = tokio_util::io::ReaderStream::new(file.take(spec.len()));
        let content_range = format!("bytes {}-{}/{file_size}", spec.start, spec.end);
        let content_length = spec.len().to_string();
        return Ok((
            StatusCode::PARTIAL_CONTENT,
            [
                (header::CONTENT_TYPE, content_type.to_string()),
                (header::CONTENT_RANGE, content_range),
                (header::CONTENT_LENGTH, content_length),
                (header::ACCEPT_RANGES, "bytes".to_string()),
            ],
            Body::from_stream(stream),
        )
            .into_response());
    }

    let stream = tokio_util::io::ReaderStream::new(file);
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CONTENT_LENGTH, file_size.to_string()),
            (header::ACCEPT_RANGES, "bytes".to_string()),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

fn video_content_type(path: &FsPath) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("mkv") => "video/x-matroska",
        Some(extension) if extension.eq_ignore_ascii_case("mp4") => "video/mp4",
        Some(extension) if extension.eq_ignore_ascii_case("m4v") => "video/x-m4v",
        Some(extension) if extension.eq_ignore_ascii_case("webm") => "video/webm",
        Some(extension) if extension.eq_ignore_ascii_case("avi") => "video/x-msvideo",
        Some(extension) if extension.eq_ignore_ascii_case("mov") => "video/quicktime",
        Some(extension) if extension.eq_ignore_ascii_case("ts") => "video/mp2t",
        Some(extension) if extension.eq_ignore_ascii_case("mpg") => "video/mpeg",
        Some(extension) if extension.eq_ignore_ascii_case("mpeg") => "video/mpeg",
        Some(extension) if extension.eq_ignore_ascii_case("wmv") => "video/x-ms-wmv",
        Some(extension) if extension.eq_ignore_ascii_case("flv") => "video/x-flv",
        _ => "application/octet-stream",
    }
}

pub async fn playback_info(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let snapshot = state
        .provider
        .resolve_single_item(user.id, &id)
        .await
        .map_err(|error| {
            tracing::error!(%error, user_id = %user.id, item_id = %id, "failed to resolve Jellyfin playback item");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;
    let is_strm = snapshot.row.path.to_ascii_lowercase().ends_with(".strm");
    let direct_url = is_strm
        .then(|| library::read_strm_url(FsPath::new(&snapshot.row.path)))
        .flatten();
    if is_strm && direct_url.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let source_path = direct_url.as_deref().unwrap_or(snapshot.row.path.as_str());
    let protocol = if source_path.starts_with("http://") || source_path.starts_with("https://") {
        "Http"
    } else {
        "File"
    };
    let container = source_path
        .split(['?', '#'])
        .next()
        .and_then(|path| FsPath::new(path).extension())
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let runtime_ticks = snapshot.metadata.runtime_ticks;
    let media_streams = dto::media_streams_with_item(&snapshot.metadata.tracks, Some(&id));
    let mut source = json!({
        "Id": id,
        "Path": source_path,
        "Protocol": protocol,
        "Container": container,
        "RunTimeTicks": runtime_ticks,
        "MediaStreams": media_streams,
        "IsRemote": is_strm,
        "SupportsDirectPlay": true,
        "SupportsDirectStream": true,
        "SupportsTranscoding": false,
    });
    if let Some(name) = snapshot.metadata.episode_name.as_ref() {
        source["Name"] = json!(name);
    }
    let client = user.playback_client(&headers);
    let play_session_id = state
        .provider
        .issue_play_session(user.id, &client.device_id, &id)
        .await
        .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
    Ok(Json(json!({
        "MediaSources": [source],
        "PlaySessionId": play_session_id,
        "Chapters": dto::render_chapters_json(&snapshot),
    })))
}

#[cfg(test)]
mod tests {
    use super::{SESSION_TOKEN_PLACEHOLDER, log_device_id, resolve_device_id};

    const TOKEN: &str = "session-token-secret";

    #[test]
    fn log_device_id_keeps_a_real_user_device_id() {
        assert_eq!(log_device_id(Some("living-room-tv"), None, TOKEN), "living-room-tv");
    }

    #[test]
    fn log_device_id_keeps_a_real_header_device_id() {
        assert_eq!(log_device_id(None, Some("phone-1"), TOKEN), "phone-1");
    }

    #[test]
    fn log_device_id_masks_the_token_fallback() {
        // 没有 DeviceId 时撤销检查回退到 token：日志必须脱敏。
        assert_eq!(log_device_id(None, None, TOKEN), SESSION_TOKEN_PLACEHOLDER);
        assert_ne!(log_device_id(None, None, TOKEN), TOKEN);
        assert_eq!(log_device_id(Some(TOKEN), None, TOKEN), SESSION_TOKEN_PLACEHOLDER);
        assert_eq!(log_device_id(None, Some(TOKEN), TOKEN), SESSION_TOKEN_PLACEHOLDER);
    }

    #[test]
    fn resolve_device_id_still_returns_the_real_value_for_auth() {
        // 鉴权路径不得被脱敏影响：兜底就是 token 原文。
        assert_eq!(resolve_device_id(None, None, TOKEN), TOKEN);
        assert_eq!(resolve_device_id(Some("tv"), Some("phone"), TOKEN), "tv");
        assert_eq!(resolve_device_id(None, Some("phone"), TOKEN), "phone");
        // user 字段为空串时仍按原语义优先（不回退 header）。
        assert_eq!(resolve_device_id(Some(""), Some("phone"), TOKEN), "");
    }
}
