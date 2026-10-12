//! 图片 conditional GET helper：授权之后才比较 validator。
//!
//! ETag 是引号包裹的 bytes SHA-256；`If-None-Match` 命中（逗号列表、`W/` 弱比较、
//! `*`）返回 304 且空 body，否则 200 + Content-Type/Length + ETag。调用方必须
//! **先**完成可见性/授权判定，隐藏资源的 404 不能变成 304。

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

/// ETag 值（含引号）。
pub fn etag_for(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("\"{digest:x}\"")
}

fn if_none_match_hits(headers: &HeaderMap, etag: &str) -> bool {
    let Some(value) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    value.split(',').any(|candidate| {
        let candidate = candidate.trim();
        if candidate == "*" {
            return true;
        }
        candidate.strip_prefix("W/").unwrap_or(candidate) == etag
    })
}

fn private_headers(headers: &mut HeaderMap) {
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    headers.insert(
        header::VARY,
        HeaderValue::from_static("Authorization, Cookie"),
    );
}

/// 200（或 validator 命中时 304）图片响应。
pub fn conditional_image(
    headers: &HeaderMap,
    bytes: Vec<u8>,
    content_type: &str,
    private: bool,
) -> Response {
    let etag = etag_for(&bytes);
    if if_none_match_hits(headers, &etag) {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        if let Ok(value) = HeaderValue::from_str(&etag) {
            response.headers_mut().insert(header::ETAG, value);
        }
        if private {
            private_headers(response.headers_mut());
        }
        return response;
    }
    let length = bytes.len();
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = StatusCode::OK;
    let output = response.headers_mut();
    let content_type = HeaderValue::from_str(content_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    output.insert(header::CONTENT_TYPE, content_type);
    output.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).unwrap_or(HeaderValue::from_static("0")),
    );
    if let Ok(value) = HeaderValue::from_str(&etag) {
        output.insert(header::ETAG, value);
    }
    if private {
        private_headers(output);
    }
    response
}
