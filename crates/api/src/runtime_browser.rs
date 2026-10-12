//! Live Browser configuration adapter. Indexer remains unaware of API/Store.
use std::sync::Arc;

use indexer::{Browser, BrowserConfig, IndexerError};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::{Store, settings_keys as keys};

#[derive(Debug, thiserror::Error)]
pub enum BrowserSettingsError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Store(#[from] store::StoreError),
}

#[derive(Clone, Debug)]
pub struct BrowserSettings {
    cdp_enabled: bool,
    cdp_url: String,
    obscura_enabled: bool,
    obscura_url: String,
    user_agent: String,
}

impl BrowserSettings {
    pub fn load(store: &Store) -> Result<Self, BrowserSettingsError> {
        let cdp_enabled = enabled(store.get_setting(keys::CDP_SYNC_ENABLED)?)?;
        let obscura_enabled = enabled(store.get_setting(keys::OBSCURA_ENABLED)?)?;
        Ok(Self {
            cdp_enabled,
            cdp_url: store.get_setting(keys::CDP_URL)?.unwrap_or_else(|| {
                if cdp_enabled {
                    String::new()
                } else {
                    "http://127.0.0.1:9222".into()
                }
            }),
            obscura_enabled,
            obscura_url: store.get_setting(keys::OBSCURA_URL)?.unwrap_or_else(|| {
                if obscura_enabled {
                    String::new()
                } else {
                    "http://127.0.0.1:9223".into()
                }
            }),
            user_agent: store
                .get_setting(keys::GLOBAL_USER_AGENT)?
                .unwrap_or_else(|| "crawler-media/0.1.0".into()),
        })
    }

    pub fn config(&self) -> BrowserConfig {
        BrowserConfig::disabled()
            .with_cdp(self.cdp_enabled, Some(self.cdp_url.clone()))
            .with_obscura(self.obscura_enabled, Some(self.obscura_url.clone()))
    }

    fn apply(&mut self, body: &Value) -> Result<(), BrowserSettingsError> {
        if body["managed"]["enabled"] == true || body["managed_enabled"] == true {
            return Err(BrowserSettingsError::Invalid(
                "managed Chromium is unsupported; use external CDP".into(),
            ));
        }
        apply_route(body.get("cdp"), &mut self.cdp_enabled, &mut self.cdp_url)?;
        apply_route(
            body.get("obscura"),
            &mut self.obscura_enabled,
            &mut self.obscura_url,
        )?;
        if let Some(ua) = body.get("user_agent") {
            self.user_agent = ua
                .as_str()
                .ok_or_else(|| invalid("user_agent must be a string"))?
                .trim()
                .to_owned();
        }
        self.config()
            .validate()
            .map_err(|error| invalid(&error.to_string()))
    }

    pub fn response(&self) -> Value {
        let route = |enabled, url: &str| {
            json!({
                "enabled": enabled, "url": url,
                "configured": enabled && BrowserConfig::validate_endpoint(Some(url)).is_ok(),
                // Saving a URL is not a successful CDP session. Do not probe or launch on GET/PUT.
                "running": null, "usable": false, "status": "unverified",
            })
        };
        json!({
            "cdp": route(self.cdp_enabled, &self.cdp_url),
            "obscura": route(self.obscura_enabled, &self.obscura_url),
            "user_agent": self.user_agent,
            "capabilities": {
                "mode": "external-cdp-only", "managed_chromium": false,
                "managed_obscura": false, "endpoint_schemes": ["http", "https"],
                "route_precedence": ["site", "obscura", "global_cdp"],
                "automatic_cloudflare_fallback": false,
            },
        })
    }

    pub fn save(store: &Store, body: &Value) -> Result<Self, BrowserSettingsError> {
        let mut settings = Self::load(store)?;
        settings.apply(body)?;
        let values = [
            (keys::CDP_URL, settings.cdp_url.as_str()),
            (keys::OBSCURA_URL, settings.obscura_url.as_str()),
            (keys::GLOBAL_USER_AGENT, settings.user_agent.as_str()),
            (
                keys::CDP_SYNC_ENABLED,
                if settings.cdp_enabled { "1" } else { "0" },
            ),
            (
                keys::OBSCURA_ENABLED,
                if settings.obscura_enabled { "1" } else { "0" },
            ),
        ];
        let originals = values
            .iter()
            .map(|(key, _)| store.get_setting(key))
            .collect::<Result<Vec<_>, _>>()?;
        // API callers hold Store's mutex: requests see the saved/restored snapshot, not
        // intermediate writes. Rollback errors are logged; this is not a cross-process transaction.
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
            obscura_enabled = settings.obscura_enabled,
            mode = "external-cdp-only",
            "Browser routes saved; endpoint connectivity unverified"
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
        BrowserConfig::enable_in(std::path::Path::new(""))?;
    }
    load_config(&store.lock())?
        .validate()
        .inspect_err(|error| {
            tracing::error!(%error, "Browser startup configuration rejected");
        })?;
    Ok(browser.with_config_provider(move || load_config(&store.lock())))
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
