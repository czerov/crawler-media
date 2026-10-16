use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::IndexerError;
use crate::browser::PageSession;

#[path = "cdp_page_transport.rs"]
mod transport;
use transport::{TIMEOUT, Transport, failure, remaining};

/// Prefix of the `BrowserSetup` message for endpoints that cannot isolate a
/// session (no `Target.createBrowserContext`). `Browser` marks such endpoints
/// incompatible instead of running sessions in a shared cookie jar.
pub const INCOMPATIBLE_PREFIX: &str = "endpoint incompatible:";

pub struct CdpPageSession {
    base_url: String,
    target_id: String,
    /// Browser-level connection owning the per-session BrowserContext.
    browser: Mutex<Option<Transport>>,
    context_id: Option<String>,
    transport: Mutex<Option<Transport>>,
    deadline: Mutex<Option<Instant>>,
    target_close_cap: Duration,
}

impl CdpPageSession {
    fn with_transport<T>(
        &self,
        operation: impl FnOnce(&mut Transport, Instant) -> Result<T, IndexerError>,
    ) -> Result<T, IndexerError> {
        let now = Instant::now();
        let deadline = match *self.deadline.lock().unwrap_or_else(|p| p.into_inner()) {
            Some(d) => now + TIMEOUT.min(d.saturating_duration_since(now)),
            None => now + TIMEOUT,
        };
        // A stalled caller cannot make subsequent callers wait indefinitely for the mutex.
        loop {
            match self.transport.try_lock() {
                Ok(mut guard) => {
                    let transport = guard
                        .as_mut()
                        .ok_or_else(|| failure("transport", "connection not initialized"))?;
                    return operation(transport, deadline);
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err(failure("transport", "mutex poisoned"));
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(remaining(deadline)?.min(Duration::from_millis(10)));
                }
            }
        }
    }
}

impl PageSession for CdpPageSession {
    fn goto(&self, url: &str) -> Result<(), IndexerError> {
        self.with_transport(|transport, deadline| {
            transport.loaded_loaders.clear();
            let result = transport.command("Page.navigate", json!({"url": url}), deadline)?;
            if let Some(error) = result.get("errorText") {
                return Err(failure("navigation", error));
            }
            let frame_id = result.get("frameId").and_then(Value::as_str);
            if frame_id.is_none() || result["isDownload"].as_bool() == Some(true) {
                return Err(failure("navigation", "no document navigation"));
            }
            loop {
                let state = evaluate(transport, "document.readyState", deadline)?;
                let loader = result["loaderId"].as_str();
                let loaded = loader.is_none_or(|id| transport.loaded_loaders.contains(id));
                if state == "complete" && loaded {
                    return Ok(());
                }
                if state != "loading" && state != "interactive" && state != "complete" {
                    return Err(failure("document readiness", state));
                }
                std::thread::sleep(remaining(deadline)?.min(Duration::from_millis(50)));
            }
        })
    }

    fn set_site_cookies(&self, page_url: &str, header: &str) -> Result<(), IndexerError> {
        let uri: tungstenite::http::Uri = page_url
            .parse()
            .map_err(|e| failure("parse URL", e))?;
        let scheme = uri.scheme_str().unwrap_or("http");
        if scheme != "http" && scheme != "https" {
            return Err(IndexerError::BrowserSetup(format!("invalid scheme '{scheme}' for cookies")));
        }
        let host = uri
            .host()
            .ok_or_else(|| IndexerError::BrowserSetup("missing host for cookies".into()))?;
        let norm_host = crate::browser_host::normalize_host(host)?;
        let norm_host = if norm_host.contains(':') {
            format!("[{norm_host}]")
        } else {
            norm_host
        };
        let port_part = uri.port_u16().map(|p| format!(":{p}")).unwrap_or_default();
        let origin = format!("{scheme}://{norm_host}{port_part}/");
        let secure = scheme == "https";

        let mut cookies = Vec::new();
        for part in header.split(';') {
            let trimmed = part.trim();
            if let Some((name, value)) = trimmed.split_once('=') {
                let name = name.trim();
                if !name.is_empty() {
                    cookies.push(json!({
                        "name": name,
                        "value": value.trim(),
                        "url": origin,
                        "secure": secure,
                        "path": "/"
                    }));
                }
            }
        }
        if cookies.is_empty() {
            return Ok(());
        }
        tracing::debug!(cookie_count = cookies.len(), "Setting site cookies via Network.setCookies");
        self.with_transport(|transport, deadline| {
            transport.command("Network.setCookies", json!({"cookies": cookies}), deadline)?;
            Ok(())
        })
    }

    fn content(&self) -> Result<String, IndexerError> {
        self.with_transport(|transport, deadline| {
            evaluate(transport, "document.documentElement.outerHTML", deadline)
        })
    }

    fn cookie_header(&self, page_url: &str) -> Result<String, IndexerError> {
        self.with_transport(|transport, deadline| {
            let result = transport.command("Network.getCookies", json!({"urls": [page_url]}), deadline)?;
            let cookies = result["cookies"]
                .as_array()
                .ok_or_else(|| failure("getCookies", "missing cookies field"))?;
            // Deterministic duplicates: a host-only cookie (domain without a
            // leading dot) wins over a Domain= cookie of the same name.
            let mut by_name: BTreeMap<&str, (bool, &str)> = BTreeMap::new();
            for c in cookies {
                if let (Some(n), Some(v)) = (c["name"].as_str(), c["value"].as_str()) {
                    if n.is_empty() {
                        continue;
                    }
                    let host_only = !c["domain"].as_str().unwrap_or("").starts_with('.');
                    match by_name.get(n) {
                        Some((true, _)) if !host_only => {}
                        _ => {
                            by_name.insert(n, (host_only, v));
                        }
                    }
                }
            }
            if by_name.is_empty() {
                return Ok(String::new());
            }
            let header = by_name
                .iter()
                .map(|(n, (_, v))| format!("{n}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            Ok(header)
        })
    }

    fn set_deadline(&self, deadline: Instant) {
        *self.deadline.lock().unwrap_or_else(|p| p.into_inner()) = Some(deadline);
    }
}

fn evaluate(
    transport: &mut Transport,
    expression: &str,
    deadline: Instant,
) -> Result<String, IndexerError> {
    let result = transport.command(
        "Runtime.evaluate",
        json!({"expression": expression, "returnByValue": true}),
        deadline,
    )?;
    if let Some(exception) = result.get("exceptionDetails") {
        return Err(failure("evaluation exception", exception));
    }
    result["result"]["value"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| failure("evaluation", "expected string result"))
}

impl Drop for CdpPageSession {
    fn drop(&mut self) {
        if let Ok(Some(transport)) = self.transport.get_mut().map(Option::as_mut) {
            transport.close();
        }
        // Dispose the isolated BrowserContext: drops every cookie this session
        // created without touching any other context's data.
        if let (Some(context_id), Ok(Some(browser))) = (
            self.context_id.as_deref(),
            self.browser.get_mut().map(Option::as_mut),
        ) {
            let deadline = Instant::now() + self.target_close_cap;
            if let Err(error) = browser.command(
                "Target.disposeBrowserContext",
                json!({"browserContextId": context_id}),
                deadline,
            ) {
                tracing::warn!(%error, "CDP browser context dispose failed");
            }
            browser.close();
        }
        let close_url = format!("{}/json/close/{}", self.base_url, self.target_id);
        if let Err(error) = http_agent(self.target_close_cap)
            .put(&close_url)
            .send_empty()
        {
            tracing::warn!(target_id = %self.target_id, %error, "CDP target cleanup failed");
        }
    }
}

fn http_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .new_agent()
}

/// Open a page target. Every failure here happens before navigation, so it is
/// reported as `BrowserSetup` (or `BrowserBusy`) and callers may fall back.
pub fn open_cdp_session_until(
    cdp_url: &str,
    deadline: Instant,
    target_close_cap: Duration,
) -> Result<CdpPageSession, IndexerError> {
    open_cdp_session_inner(cdp_url, deadline, target_close_cap).map_err(into_setup)
}

pub(crate) fn into_setup(error: IndexerError) -> IndexerError {
    match error {
        IndexerError::BrowserBusy | IndexerError::BrowserSetup(_) => error,
        other => IndexerError::BrowserSetup(other.to_string()),
    }
}

fn default_port(uri: &tungstenite::http::Uri) -> u16 {
    uri.port_u16().unwrap_or(match uri.scheme_str() {
        Some("https") | Some("wss") => 443,
        _ => 80,
    })
}

fn check_same_endpoint(base: &str, ws_url: &str, cdp_url: &str) -> Result<(), IndexerError> {
    let base_uri: tungstenite::http::Uri = base.parse().map_err(|e| failure("CDP URL", e))?;
    let ws_uri: tungstenite::http::Uri = ws_url.parse().map_err(|e| failure("WS URL", e))?;
    let norm_base = crate::browser_host::normalize_host(base_uri.host().unwrap_or(""))?;
    let norm_ws = crate::browser_host::normalize_host(ws_uri.host().unwrap_or(""))?;
    if norm_base != norm_ws || default_port(&base_uri) != default_port(&ws_uri) {
        return Err(IndexerError::BrowserSetup(format!(
            "mismatched debugger url: {ws_url} for endpoint {cdp_url}"
        )));
    }
    Ok(())
}

fn open_cdp_session_inner(
    cdp_url: &str,
    deadline: Instant,
    target_close_cap: Duration,
) -> Result<CdpPageSession, IndexerError> {
    let base = cdp_url.trim_end_matches('/');
    let now = Instant::now();
    let timeout = TIMEOUT.min(deadline.saturating_duration_since(now));
    if timeout.is_zero() {
        return Err(IndexerError::BrowserBusy);
    }
    // 1. Browser-level endpoint.
    let version: Value = http_agent(timeout)
        .get(&format!("{base}/json/version"))
        .call()
        .map_err(|error| failure("无法连接 CDP 端点 /json/version", error))?
        .into_body()
        .read_json()
        .map_err(|error| failure("version JSON", error))?;
    let browser_ws = version["webSocketDebuggerUrl"]
        .as_str()
        .ok_or_else(|| failure("json/version", "missing webSocketDebuggerUrl"))?
        .to_owned();
    check_same_endpoint(base, &browser_ws, cdp_url)?;
    let mut browser = Transport::connect(&browser_ws)?;

    // 2. Isolated cookie jar per session. Without it the endpoint is incompatible.
    let context = browser
        .command("Target.createBrowserContext", json!({"disposeOnDetach": true}), deadline)
        .map_err(|error| {
            IndexerError::BrowserSetup(format!(
                "{INCOMPATIBLE_PREFIX} Target.createBrowserContext unsupported ({error})"
            ))
        })?;
    let context_id = context["browserContextId"]
        .as_str()
        .ok_or_else(|| {
            IndexerError::BrowserSetup(format!("{INCOMPATIBLE_PREFIX} no browserContextId returned"))
        })?
        .to_owned();
    let target = match browser.command(
        "Target.createTarget",
        json!({"url": "about:blank", "browserContextId": context_id}),
        deadline,
    ) {
        Ok(target) => target,
        Err(error) => {
            let _ = browser.command(
                "Target.disposeBrowserContext",
                json!({"browserContextId": context_id}),
                Instant::now() + target_close_cap,
            );
            return Err(error);
        }
    };
    let target_id = target["targetId"].as_str().map(str::to_owned);

    let mut session = CdpPageSession {
        base_url: base.to_owned(),
        target_id: target_id.clone().unwrap_or_default(),
        browser: Mutex::new(Some(browser)),
        context_id: Some(context_id),
        transport: Mutex::new(None),
        deadline: Mutex::new(Some(deadline)),
        target_close_cap,
    };
    let target_id = target_id.ok_or_else(|| failure("create target", "missing targetId"))?;

    // 3. Page connection on the same host:port as the endpoint.
    let base_uri: tungstenite::http::Uri = base.parse().map_err(|e| failure("CDP URL", e))?;
    let authority = base_uri
        .authority()
        .ok_or_else(|| failure("CDP URL", "missing authority"))?;
    let ws_url = format!("ws://{authority}/devtools/page/{target_id}");
    check_same_endpoint(base, &ws_url, cdp_url)?;

    let mut transport = Transport::connect(&ws_url)?;
    transport.command("Network.enable", json!({}), deadline)?;
    transport.command("Page.enable", json!({}), deadline)?;
    transport.command(
        "Page.setLifecycleEventsEnabled",
        json!({"enabled": true}),
        deadline,
    )?;
    *session
        .transport
        .get_mut()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(transport);
    tracing::debug!(target_id = %session.target_id, "CDP page session opened in isolated context");
    Ok(session)
}

pub fn open_cdp_session(cdp_url: &str) -> Result<CdpPageSession, IndexerError> {
    open_cdp_session_until(cdp_url, Instant::now() + TIMEOUT, Duration::from_secs(2))
}
