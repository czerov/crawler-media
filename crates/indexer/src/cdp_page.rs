use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::IndexerError;
use crate::browser::PageSession;

#[path = "cdp_page_transport.rs"]
mod transport;
use transport::{TIMEOUT, Transport, failure, remaining};

pub struct CdpPageSession {
    base_url: String,
    target_id: String,
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
        let deadline = match *self.deadline.lock().unwrap() {
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
            let mut by_name: BTreeMap<&str, &str> = BTreeMap::new();
            for c in cookies {
                if let (Some(n), Some(v)) = (c["name"].as_str(), c["value"].as_str()) {
                    if !n.is_empty() {
                        by_name.insert(n, v);
                    }
                }
            }
            if by_name.is_empty() {
                return Ok(String::new());
            }
            let header = by_name
                .iter()
                .map(|(n, v)| format!("{n}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            Ok(header)
        })
    }

    fn set_deadline(&self, deadline: Instant) {
        *self.deadline.lock().unwrap() = Some(deadline);
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

pub fn open_cdp_session_until(
    cdp_url: &str,
    deadline: Instant,
    target_close_cap: Duration,
) -> Result<CdpPageSession, IndexerError> {
    let base = cdp_url.trim_end_matches('/');
    let new_url = format!("{base}/json/new");
    let now = Instant::now();
    let timeout = TIMEOUT.min(deadline.saturating_duration_since(now));
    if timeout.is_zero() {
        return Err(IndexerError::BrowserBusy);
    }
    let response: Value = http_agent(timeout)
        .put(&new_url)
        .send_empty()
        .map_err(|error| failure("无法连接外部 CDP 端点创建 target", error))?
        .into_body()
        .read_json()
        .map_err(|error| failure("target JSON", error))?;
    let target_id = response["id"]
        .as_str()
        .ok_or_else(|| failure("create target", "missing target id"))?
        .to_owned();

    let mut session = CdpPageSession {
        base_url: base.to_owned(),
        target_id,
        transport: Mutex::new(None),
        deadline: Mutex::new(Some(deadline)),
        target_close_cap,
    };

    let ws_url = response["webSocketDebuggerUrl"]
        .as_str()
        .ok_or_else(|| failure("create target", "missing WebSocket URL"))?;

    // Host & Port matching validation
    let base_uri: tungstenite::http::Uri = base.parse().map_err(|e| failure("CDP URL", e))?;
    let ws_uri: tungstenite::http::Uri = ws_url.parse().map_err(|e| failure("WS URL", e))?;
    let base_host = base_uri.host().unwrap_or("");
    let ws_host = ws_uri.host().unwrap_or("");
    let base_port = base_uri.port_u16().unwrap_or(80);
    let ws_port = ws_uri.port_u16().unwrap_or(80);
    let norm_base = crate::browser_host::normalize_host(base_host)?;
    let norm_ws = crate::browser_host::normalize_host(ws_host)?;
    if norm_base != norm_ws || base_port != ws_port {
        return Err(IndexerError::BrowserSetup(format!(
            "mismatched debugger url: {ws_url} for endpoint {cdp_url}"
        )));
    }

    let mut transport = Transport::connect(ws_url)?;
    transport.command("Network.enable", json!({}), deadline)?;
    transport.command("Page.enable", json!({}), deadline)?;
    transport.command(
        "Page.setLifecycleEventsEnabled",
        json!({"enabled": true}),
        deadline,
    )?;
    *session.transport.get_mut().unwrap() = Some(transport);
    tracing::debug!(target_id = %session.target_id, "CDP page session opened");
    Ok(session)
}

pub fn open_cdp_session(cdp_url: &str) -> Result<CdpPageSession, IndexerError> {
    open_cdp_session_until(cdp_url, Instant::now() + TIMEOUT, Duration::from_secs(2))
}
