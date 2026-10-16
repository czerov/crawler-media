//! Live Browser configuration adapter. Indexer remains unaware of API/Store.
use std::sync::Arc;

use indexer::{Browser, BrowserConfig, IndexerError};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::{Store, settings_keys as keys};

#[derive(Debug, thiserror::Error)]
pub enum BrowserSettingsError {
    #[error("{0}")]
    InvalidEndpoint(String),
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Store(#[from] store::StoreError),
}

#[derive(Clone, Debug)]
pub struct BrowserSettings {
    cdp_enabled: bool,
    cdp_url: String,
    obscura_install: String,
    user_agent: String,
}

impl BrowserSettings {
    pub fn load(store: &Store) -> Result<Self, BrowserSettingsError> {
        let cdp_enabled = enabled(store.get_setting(keys::CDP_ENABLED)?)?;
        let cdp_url = store.get_setting(keys::CDP_URL)?.unwrap_or_default();
        let obscura_install = store
            .get_setting(keys::OBSCURA_INSTALL)?
            .unwrap_or_else(|| "preinstalled".into());
        let user_agent = store
            .get_setting(keys::GLOBAL_USER_AGENT)?
            .unwrap_or_else(|| "crawler-media/0.1.0".into());
        Ok(Self {
            cdp_enabled,
            cdp_url,
            obscura_install,
            user_agent,
        })
    }

    pub fn cdp_enabled(&self) -> bool {
        self.cdp_enabled
    }

    pub fn cdp_url(&self) -> &str {
        &self.cdp_url
    }

    pub fn obscura_install(&self) -> &str {
        &self.obscura_install
    }

    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    pub fn config(&self) -> BrowserConfig {
        BrowserConfig {
            enabled: self.cdp_enabled,
            endpoint: if self.cdp_url.trim().is_empty() {
                None
            } else {
                Some(self.cdp_url.trim().to_string())
            },
            managed_endpoint: None,
        }
    }

    fn apply(&mut self, body: &Value) -> Result<(), BrowserSettingsError> {
        apply_route(body.get("cdp"), &mut self.cdp_enabled, &mut self.cdp_url)?;
        if let Some(obs) = body.get("obscura") {
            if let Some(install) = obs.get("install").and_then(Value::as_str) {
                let inst = install.trim();
                if inst != "preinstalled" && inst != "download" {
                    return Err(invalid("obscura.install must be 'preinstalled' or 'download'"));
                }
                self.obscura_install = inst.to_string();
            }
        }
        if let Some(ua) = body.get("user_agent") {
            self.user_agent = ua
                .as_str()
                .ok_or_else(|| invalid("user_agent must be a string"))?
                .trim()
                .to_owned();
        }
        if self.cdp_enabled && !self.cdp_url.trim().is_empty() {
            BrowserConfig::validate_endpoint(&self.cdp_url)
                .map_err(|error| BrowserSettingsError::InvalidEndpoint(error.to_string()))?;
        }
        Ok(())
    }

    pub fn response(&self) -> Value {
        let route = if !self.cdp_enabled {
            "disabled"
        } else if self.cdp_url.trim().is_empty() {
            "managed"
        } else {
            "instance"
        };
        let configured = self.cdp_enabled
            && !self.cdp_url.trim().is_empty()
            && BrowserConfig::validate_endpoint(&self.cdp_url).is_ok();
        json!({
            "cdp": {
                "enabled": self.cdp_enabled,
                "url": self.cdp_url,
                "configured": configured,
                "route": route,
                "status": "unverified",
            },
            "user_agent": self.user_agent,
            "browser_sites": 0,
            "managed_browser_sites": 0,
            "ignored_site_endpoints": 0,
            "capabilities": {
                "mode": "cdp-opt-in",
                "managed_chromium": false,
                "managed_obscura": true,
                "endpoint_schemes": ["http"],
                "route_precedence": ["site", "instance", "managed"],
                "uses": ["login", "check_in"],
                "search": "http_only",
                "fallback": "http_before_navigation",
                "automatic_cloudflare_fallback": false,
            },
        })
    }

    pub fn response_with_store(&self, store: &Store) -> Value {
        let mut resp = self.response();
        let sites = store.list_sites().unwrap_or_default();
        let mut browser_sites = 0;
        let mut managed_browser_sites = 0;
        let mut ignored_site_endpoints = 0;

        for s in &sites {
            let is_browser = s.enabled && s.maintenance_via == domain::MaintenanceVia::Browser;
            if is_browser {
                browser_sites += 1;
                if s.cdp_url.as_deref().unwrap_or("").trim().is_empty() {
                    managed_browser_sites += 1;
                }
            }
            let has_cdp = s.cdp_url.as_deref().map(|u| !u.trim().is_empty()).unwrap_or(false);
            if (!self.cdp_enabled || s.maintenance_via == domain::MaintenanceVia::Http) && has_cdp {
                ignored_site_endpoints += 1;
            }
        }
        resp["browser_sites"] = json!(browser_sites);
        resp["managed_browser_sites"] = json!(managed_browser_sites);
        resp["ignored_site_endpoints"] = json!(ignored_site_endpoints);
        resp
    }

    pub fn save(store: &Store, body: &Value) -> Result<Self, BrowserSettingsError> {
        let mut settings = Self::load(store)?;
        settings.apply(body)?;
        let values = [
            (keys::CDP_URL, settings.cdp_url.as_str()),
            (keys::OBSCURA_INSTALL, settings.obscura_install.as_str()),
            (keys::GLOBAL_USER_AGENT, settings.user_agent.as_str()),
            (
                keys::CDP_ENABLED,
                if settings.cdp_enabled { "1" } else { "0" },
            ),
        ];
        let originals = values
            .iter()
            .map(|(key, _)| store.get_setting(key))
            .collect::<Result<Vec<_>, _>>()?;

        for (index, (key, value)) in values.iter().enumerate() {
            if let Err(error) = store.put_setting(key, value) {
                for ((old_key, _), old_value) in values[..index].iter().zip(&originals) {
                    let restored = match old_value {
                        Some(value) => store.put_setting(old_key, value),
                        None => store.delete_setting(old_key).map(|_| ()),
                    };
                    if let Err(rollback_error) = restored {
                        tracing::error!(%rollback_error, key = old_key, "Browser settings rollback failed");
                    }
                }
                tracing::error!(%error, key, "Browser settings save failed");
                return Err(error.into());
            }
        }
        tracing::info!(
            cdp_enabled = settings.cdp_enabled,
            mode = "cdp-opt-in",
            "Browser settings saved"
        );
        Ok(settings)
    }
}

fn invalid(message: &str) -> BrowserSettingsError {
    BrowserSettingsError::Invalid(message.to_owned())
}

/// Settings-KV flag (`1`/`true`/`yes`/`on`, case-insensitive). An unknown value
/// is an error rather than a silent "off", so a typo cannot disable a route.
fn enabled(value: Option<String>) -> Result<bool, BrowserSettingsError> {
    match value.as_deref() {
        None => Ok(false),
        Some(raw) if raw.trim().is_empty() => Ok(false),
        Some(raw) => {
            crate::config::parse_bool(raw).ok_or_else(|| invalid("invalid Browser enabled flag"))
        }
    }
}

fn apply_route(
    value: Option<&Value>,
    enabled: &mut bool,
    url: &mut String,
) -> Result<(), BrowserSettingsError> {
    let Some(value) = value else {
        return Ok(());
    };
    if !value.is_object() {
        return Err(invalid("Browser route must be an object"));
    }
    if let Some(flag) = value.get("enabled") {
        *enabled = flag
            .as_bool()
            .ok_or_else(|| invalid("enabled must be a boolean"))?;
    }
    if let Some(endpoint) = value.get("url") {
        *url = endpoint
            .as_str()
            .ok_or_else(|| invalid("CDP URL must be a string"))?
            .trim()
            .to_owned();
    }
    Ok(())
}

/// Shared startup and request-time configuration path, also usable with an injected transport.
pub fn configure(
    browser: Browser,
    store: Arc<Mutex<Store>>,
    managed: bool,
) -> Result<Browser, IndexerError> {
    if managed {
        // legacy flag
    }
    browser.validate_timing()?;
    load_config(&store.lock())?;
    Ok(browser.with_config_provider(move || load_config(&store.lock())))
}

/// Startup path: never silently swap in a permanently disabled Browser. A bad
/// initial config is logged and returned as an error string for status; the
/// returned Browser still reads settings on every use, so fixing them in the
/// UI takes effect without a restart.
pub fn startup_browser(store: Arc<Mutex<Store>>) -> (Browser, Option<String>) {
    if store.lock().get_setting("cdp.sync.enabled").ok().flatten().is_some() {
        tracing::warn!(
            "Legacy setting cdp.sync.enabled is ignored; enable the Browser switch in settings \
             and choose maintenance_via=browser per Site if needed"
        );
    }
    match production_browser(store.clone(), false) {
        Ok(browser) => (browser, None),
        Err(error) => {
            tracing::error!(%error, "Browser construction failed at startup; settings will be re-read on use");
            let browser = Browser::new(BrowserConfig::disabled())
                .with_config_provider(move || load_config(&store.lock()));
            (browser, Some(format!("Browser 初始化失败: {error}")))
        }
    }
}

pub fn production_browser(
    store: Arc<Mutex<Store>>,
    managed: bool,
) -> Result<Browser, IndexerError> {
    configure(Browser::new(BrowserConfig::disabled()), store, managed)
}

fn load_config(store: &Store) -> Result<BrowserConfig, IndexerError> {
    BrowserSettings::load(store)
        .map(|settings| settings.config())
        .map_err(|error| {
            tracing::error!(%error, "Browser configuration read failed");
            IndexerError::Fetch(format!("Browser configuration unavailable: {error}"))
        })
}
