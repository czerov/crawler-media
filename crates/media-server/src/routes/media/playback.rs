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

async fn check_device_revocation(
    provider: &dyn MediaServerProvider,
    user: &AuthUser,
    headers: &HeaderMap,
    action: &str,
) -> Result<(), StatusCode> {
    let device_id = user
        .device_id
        .as_deref()
        .or_else(|| {
            headers
                .get("X-Emby-Device-Id")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|v| !v.is_empty())
        })
        .unwrap_or(&user.token);

    match provider.is_device_revoked(user.id, device_id).await {
        Ok(true) => {
            tracing::warn!(user_id = %user.id, device_id, "{action}请求被阻断：设备已撤销");
            return Err(StatusCode::FORBIDDEN);
        }
        Ok(false) => {}
        Err(err) => {
            tracing::error!(user_id = %user.id, device_id, error = %err, "检查设备撤销状态失败");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    if device_id != user.token.as_str() {
        match provider.is_device_revoked(user.id, &user.token).await {
            Ok(true) => {
                tracing::warn!(user_id = %user.id, token = %user.token, "{action}请求被阻断：会话Token关联设备已撤销");
                return Err(StatusCode::FORBIDDEN);
            }
            Ok(false) => {}
            Err(err) => {
                tracing::error!(user_id = %user.id, token = %user.token, error = %err, "检查Token撤销状态失败");
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
    Ok(Json(json!({
        "MediaSources": [source],
        "PlaySessionId": uuid::Uuid::new_v4().to_string(),
        "Chapters": dto::render_chapters_json(&snapshot),
    })))
}
