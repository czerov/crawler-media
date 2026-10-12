//! Request observation: pure latency/route/request-id computation and query
//! redaction, used by the HTTP middleware in [`crate::http`].

use std::time::Duration;

/// One request's observation. `headers_latency_ms` stops when the response
/// **headers** are ready — it is **not** the full body/stream transfer time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RequestObservation {
    pub method: String,
    pub route: String,
    pub request_id: String,
    pub status: u16,
    pub headers_latency_ms: u128,
    pub slow: bool,
}

/// Pure: fold elapsed time and the configured threshold into an observation.
/// `slow_after == 0` disables the slow-request escalation.
pub(crate) fn observe(
    method: &str,
    route: &str,
    request_id: &str,
    status: u16,
    elapsed: Duration,
    slow_after: Duration,
) -> RequestObservation {
    RequestObservation {
        method: method.to_string(),
        route: route.to_string(),
        request_id: request_id.to_string(),
        status,
        headers_latency_ms: elapsed.as_millis(),
        slow: !slow_after.is_zero() && elapsed >= slow_after,
    }
}

/// Query parameter names whose values must never reach the logs (matched
/// case-insensitively; percent-encoded names are already decoded by the parser).
const SECRET_QUERY_KEYS: &[&str] = &[
    "api_key",
    "apikey",
    "api-key",
    "token",
    "access_token",
    "access-token",
    "refresh_token",
    "password",
    "passwd",
    "secret",
    "api_secret",
    "authorization",
];

fn is_secret_query_key(key: &str) -> bool {
    SECRET_QUERY_KEYS.contains(&key.to_ascii_lowercase().as_str())
}

/// Parse with `url::form_urlencoded` (no hand-rolled splitting) and rebuild the
/// query string with secret values replaced by `[redacted]`.
pub(crate) fn redact_query(query: &str) -> String {
    let mut out = String::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if !out.is_empty() {
            out.push('&');
        }
        if is_secret_query_key(&key) {
            out.push_str(&key);
            out.push_str("=[redacted]");
        } else {
            out.push_str(&key);
            out.push('=');
            out.push_str(&value);
        }
    }
    out
}

/// Correlation id: echo a caller-supplied `X-Request-Id` only when it is a short
/// ASCII-safe token, otherwise mint a fresh UUID. A bearer token is never a
/// valid id (too long / wrong charset), so it can never be echoed back.
pub(crate) fn request_id_from(headers: &axum::http::HeaderMap) -> String {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| is_safe_request_id(value))
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

fn is_safe_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// `/health`、SSE 与日志接口本身不记录，避免自激。
pub(crate) fn is_observed(path: &str) -> bool {
    !path.starts_with("/system/logs") && !path.ends_with("/stream") && path != "/health"
}

/// Map an observation to a log level: 5xx → error, 4xx → warn (optional-cover
/// 404 stays debug), over-threshold → warn, otherwise debug.
pub(crate) fn log_request(
    observation: &RequestObservation,
    path: &str,
    query: Option<&str>,
    user_agent: Option<&str>,
) {
    if !is_observed(path) {
        return;
    }
    let RequestObservation {
        method,
        route,
        request_id,
        status,
        headers_latency_ms,
        slow,
    } = observation;
    if *status >= 500 {
        tracing::error!(
            method = %method,
            route = %route,
            request_id = %request_id,
            status = status,
            headers_latency_ms = headers_latency_ms,
            slow = slow,
            query,
            user_agent,
            "HTTP 请求失败（5xx）"
        );
    } else if *status >= 400 {
        // 404 Not Found 对于可选图片静态资源（如未配封面的媒体库、无剧照的分集等）
        // 属于预期探测行为，打 debug 即可，避免在正常浏览时刷出误导性的 WARN 日志。
        if *status == 404 && route.ends_with("/cover") {
            tracing::debug!(
                method = %method,
                route = %route,
                request_id = %request_id,
                status = status,
                headers_latency_ms = headers_latency_ms,
                query,
                "可选封面不存在"
            );
        } else {
            tracing::warn!(
                method = %method,
                route = %route,
                request_id = %request_id,
                status = status,
                headers_latency_ms = headers_latency_ms,
                slow = slow,
                query,
                user_agent,
                "HTTP 请求异常（4xx）"
            );
        }
    } else if *slow {
        tracing::warn!(
            method = %method,
            route = %route,
            request_id = %request_id,
            status = status,
            headers_latency_ms = headers_latency_ms,
            slow = slow,
            query,
            user_agent,
            "HTTP 请求缓慢"
        );
    } else {
        tracing::debug!(
            method = %method,
            route = %route,
            request_id = %request_id,
            status = status,
            headers_latency_ms = headers_latency_ms,
            query,
            user_agent,
            "HTTP 请求"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{observe, redact_query, request_id_from};
    use std::time::Duration;

    #[test]
    fn observe_marks_requests_at_or_over_the_threshold_as_slow() {
        let threshold = Duration::from_millis(1_000);
        let slow = observe("GET", "/x", "id", 200, Duration::from_millis(1_500), threshold);
        assert!(slow.slow);
        assert_eq!(slow.headers_latency_ms, 1_500);
        assert_eq!(slow.status, 200);

        let edge = observe("GET", "/x", "id", 200, Duration::from_millis(1_000), threshold);
        assert!(edge.slow, "恰好等于阈值也算慢");

        let fast = observe("GET", "/x", "id", 200, Duration::from_millis(900), threshold);
        assert!(!fast.slow);
    }

    #[test]
    fn zero_threshold_disables_the_slow_escalation() {
        let observation = observe(
            "GET",
            "/x",
            "id",
            200,
            Duration::from_secs(30),
            Duration::ZERO,
        );
        assert!(!observation.slow);
        assert_eq!(observation.headers_latency_ms, 30_000);
    }

    #[test]
    fn redact_query_masks_plain_and_percent_encoded_secret_keys() {
        assert_eq!(redact_query("token=abc"), "token=[redacted]");
        assert_eq!(redact_query("%61pi_key=abc"), "api_key=[redacted]");
        assert_eq!(redact_query("API_KEY=abc"), "API_KEY=[redacted]");
        assert_eq!(
            redact_query("q=hello&Access_Token=abc&page=2"),
            "q=hello&Access_Token=[redacted]&page=2"
        );
        assert_eq!(redact_query("q=hello&page=2"), "q=hello&page=2");
        assert_eq!(redact_query(""), "");
    }

    #[test]
    fn request_id_echoes_safe_values_and_replaces_unsafe_ones() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-request-id", "abc-123_XY.z".parse().unwrap());
        assert_eq!(request_id_from(&headers), "abc-123_XY.z");

        for unsafe_value in ["", "has space", "长中文", &"a".repeat(65)] {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert("x-request-id", unsafe_value.parse().unwrap());
            let generated = request_id_from(&headers);
            assert_ne!(generated, unsafe_value);
            assert_eq!(generated.len(), 36, "回退必须是 UUID");
        }
    }
}
