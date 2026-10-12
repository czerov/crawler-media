use std::collections::HashMap;
use std::convert::Infallible;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_core::Stream;
use serde_json::json;
use tokio_stream::StreamExt;
use tokio_util::io::ReaderStream;
use tokio_stream::wrappers::BroadcastStream;

use crate::http::ok;
use crate::management::ApiState;

/// Query parameters for GET /api/v1/system/logs
pub(crate) async fn list_logs(
    State(state): State<ApiState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let level = query.get("level").map(String::as_str);
    let target = query
        .get("target")
        .or_else(|| query.get("module"))
        .map(String::as_str);
    let keyword = query
        .get("q")
        .or_else(|| query.get("keyword"))
        .map(String::as_str);
    let limit = query.get("limit").and_then(|l| l.parse::<usize>().ok());

    let entries = state.log_buffer.query(level, target, keyword, limit);
    ok(json!({
        "total": entries.len(),
        "entries": entries,
    }))
    .into_response()
}

/// Clear in-memory log buffer
pub(crate) async fn clear_logs(State(state): State<ApiState>) -> Response {
    state.log_buffer.clear();
    ok(json!({ "cleared": true })).into_response()
}

/// Export logs as plain text.
///
/// 目录选择放在 blocking 线程（保持既有「最新日志文件 + 缓冲回退」语义），
/// 命中的文件用流式读取返回，不再把整个文件读进内存。
pub(crate) async fn export_logs(State(state): State<ApiState>) -> Response {
    let data_dir = {
        let store = state.store.lock();
        store.data_dir().to_path_buf()
    };
    let logs_dir = data_dir.join("logs");
    let selected = tokio::task::spawn_blocking(move || latest_log_file(&logs_dir))
        .await
        .unwrap_or(None);
    let filename = format!("crawler-media-logs-{}.txt", chrono_like_timestamp());
    let Some(path) = selected else {
        return buffer_export(&state, &filename);
    };
    let file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "打开日志文件失败，回退内存缓冲");
            return buffer_export(&state, &filename);
        }
    };
    let length = file.metadata().await.ok().map(|meta| meta.len());
    let mut response = Response::new(Body::from_stream(ReaderStream::new(file)));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    if let Ok(value) = axum::http::HeaderValue::from_str(&content_disposition(&filename)) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    if let Some(length) = length {
        headers.insert(header::CONTENT_LENGTH, axum::http::HeaderValue::from(length));
    }
    response
}

/// 既有语义：`logs/` 下 `crawler-media.log*` 里字典序最大的文件。
fn latest_log_file(logs_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    if !logs_dir.is_dir() {
        return None;
    }
    let mut files: Vec<_> = std::fs::read_dir(logs_dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("crawler-media.log"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    files.pop()
}

fn content_disposition(filename: &str) -> String {
    format!(r#"attachment; filename="{filename}""#)
}

/// 内存缓冲回退（≤3000 条，行为保持原样）。
fn buffer_export(state: &ApiState, filename: &str) -> Response {
    let content = export_from_buffer(state);
    let mut response = content.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    if let Ok(value) = axum::http::HeaderValue::from_str(&content_disposition(filename)) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    response
}

fn export_from_buffer(state: &ApiState) -> String {
    let entries = state.log_buffer.query(None, None, None, None);
    let mut out = String::new();
    for entry in entries {
        out.push_str(&format!(
            "[{}] [{}] [{}] {}\n",
            format_timestamp(entry.timestamp),
            entry.level,
            entry.target,
            entry.message
        ));
    }
    out
}

fn format_timestamp(ms: i64) -> String {
    // Simple ISO-ish representation without external chrono dependency
    let secs = ms / 1000;
    let millis = ms % 1000;
    format!("{secs}.{millis:03}")
}

fn chrono_like_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// SSE stream for real-time logs
pub(crate) async fn stream_logs(
    State(state): State<ApiState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.log_buffer.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|item| match item {
        Ok(entry) => {
            let data = serde_json::to_string(&entry).unwrap_or_default();
            Some(Ok(Event::default().event("log").data(data)))
        }
        Err(_) => None,
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}
