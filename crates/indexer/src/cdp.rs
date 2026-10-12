use crate::IndexerError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Clone, Deserialize, Serialize)]
pub struct CdpCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub expires: Option<f64>,
    #[serde(default)]
    pub http_only: Option<bool>,
    #[serde(default)]
    pub secure: Option<bool>,
}

/// 手写 `Debug`：`value` 是 cookie 明文（站点会话凭证），绝不能进日志。
///
/// 之前这里是 `#[derive(Debug)]`，任何一句 `debug!(?cookie)` 都会把 cookie 明文
/// 打进日志与前端"系统日志"。`Serialize`/`Deserialize` 逐字保留（CDP 原始
/// 响应要能往返），其余字段原样，便于诊断。
impl std::fmt::Debug for CdpCookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CdpCookie")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .field("domain", &self.domain)
            .field("path", &self.path)
            .field("expires", &self.expires)
            .field("http_only", &self.http_only)
            .field("secure", &self.secure)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DiscoveredSiteCookie {
    pub profile_id: String,
    pub domain: String,
    pub cookie_header: String,
    pub cookie_count: usize,
}

/// 手写 `Debug`：`cookie_header` 是站点会话凭证，绝不能进日志。
///
/// 之前这里是 `#[derive(Debug)]`，任何一句 `debug!(?discovered)` 都会把 cookie
/// 明文打进日志与前端"系统日志"。`Serialize`/`Deserialize` 逐字保留（CDP 同步
/// 结果要给前端），其余字段原样，便于诊断。
impl std::fmt::Debug for DiscoveredSiteCookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoveredSiteCookie")
            .field("profile_id", &self.profile_id)
            .field("domain", &self.domain)
            .field("cookie_header", &"<redacted>")
            .field("cookie_count", &self.cookie_count)
            .finish()
    }
}

#[cfg(test)]
mod debug_tests {
    use super::{CdpCookie, DiscoveredSiteCookie};

    #[test]
    fn debug_never_prints_the_cdp_cookie_value() {
        let cookie = CdpCookie {
            name: "c_secure_pass".into(),
            value: "cdp-cookie-value-secret".into(),
            domain: ".pt.example".into(),
            path: Some("/".into()),
            expires: None,
            http_only: Some(true),
            secure: Some(true),
        };
        let rendered = format!("{cookie:?}");
        assert!(
            !rendered.contains("cdp-cookie-value-secret"),
            "Debug 不得输出 cookie value: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("c_secure_pass"), "{rendered}");
        assert!(rendered.contains("pt.example"), "{rendered}");
    }

    #[test]
    fn debug_never_prints_the_discovered_cookie_header() {
        let discovered = DiscoveredSiteCookie {
            profile_id: "pterclub".into(),
            domain: "pterclub.net".into(),
            cookie_header: "uid=1; pass=cookie-header-secret".into(),
            cookie_count: 2,
        };
        let rendered = format!("{discovered:?}");
        assert!(
            !rendered.contains("cookie-header-secret"),
            "Debug 不得输出 cookie_header: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("pterclub"), "{rendered}");
        assert!(rendered.contains("cookie_count: 2"), "非凭证字段保留: {rendered}");
    }
}

fn cdp_http_agent() -> ureq::Agent {
    let config = ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .build();
    ureq::Agent::new_with_config(config)
}

/// Query Chrome / Chromium remote debugging endpoint for all cookies across all tabs/contexts.
/// Communicates via HTTP /json endpoints or DevTools WebSocket JSON-RPC.
pub fn fetch_cookies_from_cdp(cdp_url: &str) -> Result<Vec<CdpCookie>, IndexerError> {
    let base = cdp_url.trim_end_matches('/');

    // 1. First probe http://<host>:<port>/json/version to get the browser webSocketDebuggerUrl
    let version_url = format!("{base}/json/version");
    let resp: serde_json::Value = cdp_http_agent()
        .get(&version_url)
        .call()
        .map_err(|e| IndexerError::Fetch(format!("无法连接 CDP 端口 {version_url}: {e}")))?
        .into_body()
        .read_json()
        .map_err(|e| IndexerError::Fetch(format!("CDP 响应不是合法 JSON: {e}")))?;

    let ws_url = resp["webSocketDebuggerUrl"].as_str().ok_or_else(|| {
        IndexerError::Fetch("CDP version 响应中未包含 webSocketDebuggerUrl".into())
    })?;

    // 2. Connect to the browser-level WebSocket and execute Network.getAllCookies
    fetch_cookies_over_ws(ws_url)
}

fn fetch_cookies_over_ws(ws_url: &str) -> Result<Vec<CdpCookie>, IndexerError> {
    use tungstenite::Message;
    use tungstenite::client::connect;

    let (mut socket, _) = connect(ws_url)
        .map_err(|e| IndexerError::Fetch(format!("连接 CDP WebSocket 失败 ({ws_url}): {e}")))?;

    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_ref() {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    }

    // Send Network.getAllCookies
    let req = serde_json::json!({
        "id": 1001,
        "method": "Network.getAllCookies",
        "params": {}
    });

    socket
        .send(Message::Text(req.to_string().into()))
        .map_err(|e| IndexerError::Fetch(format!("向 CDP 发送 Network.getAllCookies 失败: {e}")))?;

    // Read until response id == 1001
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        let msg = socket
            .read()
            .map_err(|e| IndexerError::Fetch(format!("读取 CDP 消息失败: {e}")))?;

        if let Message::Text(text) = msg {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                if val["id"] == 1001 {
                    if let Some(cookies_val) = val["result"]["cookies"].as_array() {
                        let mut cookies = Vec::new();
                        for item in cookies_val {
                            if let Ok(cookie) = serde_json::from_value::<CdpCookie>(item.clone()) {
                                cookies.push(cookie);
                            }
                        }
                        return Ok(cookies);
                    } else if let Some(err) = val.get("error") {
                        return Err(IndexerError::Fetch(format!("CDP 返回错误: {err}")));
                    }
                }
            }
        }
    }

    Err(IndexerError::Fetch(
        "等待 CDP 返回 cookies 超时 (5s)".into(),
    ))
}

/// Match raw cookies against a list of known site domains (from profiles).
/// Returns a map of matched profile_id -> DiscoveredSiteCookie.
pub fn match_cookies_to_profiles(
    cookies: &[CdpCookie],
    profile_domains: &[(String, String)], // (profile_id, primary_domain e.g. "pterclub.net")
) -> Vec<DiscoveredSiteCookie> {
    let mut by_domain: HashMap<String, Vec<&CdpCookie>> = HashMap::new();

    for cookie in cookies {
        let raw_domain = cookie.domain.trim_start_matches('.').to_lowercase();
        by_domain.entry(raw_domain).or_default().push(cookie);
    }

    let mut results = Vec::new();

    for (profile_id, target_domain) in profile_domains {
        let target_clean = target_domain.trim_start_matches('.').to_lowercase();
        // Match exact or suffix subdomain (e.g. .m-team.cc or kp.m-team.cc)
        let mut matched_cookies: Vec<&CdpCookie> = Vec::new();
        for (dom, list) in &by_domain {
            if dom == &target_clean || dom.ends_with(&format!(".{target_clean}")) {
                matched_cookies.extend(list);
            }
        }

        if !matched_cookies.is_empty() {
            // Deduplicate by cookie name (prefer longer/specific path or keep first)
            let mut name_map: HashMap<&str, &str> = HashMap::new();
            for c in &matched_cookies {
                name_map.insert(c.name.as_str(), c.value.as_str());
            }

            let mut pairs: Vec<String> = name_map.iter().map(|(k, v)| format!("{k}={v}")).collect();
            pairs.sort(); // 保持顺序确定性
            let cookie_header = pairs.join("; ");

            results.push(DiscoveredSiteCookie {
                profile_id: profile_id.clone(),
                domain: target_clean,
                cookie_header,
                cookie_count: name_map.len(),
            });
        }
    }

    results
}
