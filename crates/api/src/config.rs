//! Centralized environment variable registration.
//!
//! All `CRAWLER_MEDIA_*` variables are parsed here at startup.
//! Each field documents its purpose and default. Scattered `std::env::var`
//! calls elsewhere should migrate here over time.
//!
//! Parsing rules that more than one module needs live here as pure helpers
//! ([`parse_bool`], [`proxy_env`], [`secret_from`]) so a value means the same
//! thing whether it arrives from the environment, the settings KV or the CLI.

use std::path::PathBuf;

use crate::settings_keys;

/// Every `CRAWLER_MEDIA_*` variable this process reads, including `_FILE`
/// variants. Anything else prefixed `CRAWLER_MEDIA_` is almost always a typo,
/// so [`ServerConfig::from_env`] warns about it.
pub const KNOWN_ENV_VARS: &[&str] = &[
    "CRAWLER_MEDIA_DATA",
    "CRAWLER_MEDIA_TOKEN",
    "CRAWLER_MEDIA_ADMIN_PASSWORD",
    "CRAWLER_MEDIA_LISTEN",
    "CRAWLER_MEDIA_UI",
    "CRAWLER_MEDIA_CORS_ORIGINS",
    "CRAWLER_MEDIA_TMDB_KEY",
    "CRAWLER_MEDIA_TVDB_KEY",
    "CRAWLER_MEDIA_BROWSER",
    "CRAWLER_MEDIA_QB_URL",
    "CRAWLER_MEDIA_QB_USER",
    "CRAWLER_MEDIA_QB_PASS",
    "CRAWLER_MEDIA_QB_CATEGORY",
    "CRAWLER_MEDIA_QB_PATH_MAP",
    "CRAWLER_MEDIA_TR_PATH_MAP",
    "CRAWLER_MEDIA_METADATA_PROXY",
    "CRAWLER_MEDIA_METADATA_PROXY_USER",
    "CRAWLER_MEDIA_METADATA_PROXY_PASS",
    "CRAWLER_MEDIA_URL", // CLI-only
    // Read by other crates in the same process (marker/fingerprint).
    "CRAWLER_MEDIA_FFMPEG_LOG_LEVEL",
    // Secret-file variants (Docker/K8s secret volumes).
    "CRAWLER_MEDIA_TOKEN_FILE",
    "CRAWLER_MEDIA_ADMIN_PASSWORD_FILE",
    "CRAWLER_MEDIA_TMDB_KEY_FILE",
    "CRAWLER_MEDIA_TVDB_KEY_FILE",
    "CRAWLER_MEDIA_QB_PASS_FILE",
    "CRAWLER_MEDIA_METADATA_PROXY_USER_FILE",
    "CRAWLER_MEDIA_METADATA_PROXY_PASS_FILE",
];

/// All environment-sourced configuration for the server process.
#[derive(Clone)]
pub struct ServerConfig {
    /// Data directory for SQLite, cache, images, etc.
    /// Env: `CRAWLER_MEDIA_DATA`, default: `"data"`.
    pub data_dir: PathBuf,

    /// Shared authentication token (required for serve mode).
    /// Env: `CRAWLER_MEDIA_TOKEN`.
    pub token: Option<String>,

    /// Initial Administrator password (independent from CLI bearer token).
    /// Env: `CRAWLER_MEDIA_ADMIN_PASSWORD` (or `_FILE`).
    pub admin_password: Option<String>,

    /// Listen address.
    /// Env: `CRAWLER_MEDIA_LISTEN`, default: `"127.0.0.1:18765"`.
    pub listen: String,

    /// Static UI directory (production: `/usr/share/crawler-media/ui`).
    /// Env: `CRAWLER_MEDIA_UI`.
    pub ui_dir: Option<PathBuf>,

    /// CORS allowed origins (comma-separated).
    /// Env: `CRAWLER_MEDIA_CORS_ORIGINS`.
    pub cors_origins: Vec<String>,

    /// TMDB API key (seeded into settings KV on startup).
    /// Env: `CRAWLER_MEDIA_TMDB_KEY`.
    pub tmdb_key: Option<String>,

    /// TVDB API key.
    /// Env: `CRAWLER_MEDIA_TVDB_KEY`.
    pub tvdb_key: Option<String>,

    /// Enable managed Chromium browser.
    /// Env: `CRAWLER_MEDIA_BROWSER` (flag: `1`/`true`/`yes`/`on`).
    pub browser_enabled: bool,

    // -- qBittorrent --
    /// Env: `CRAWLER_MEDIA_QB_URL`.
    pub qb_url: Option<String>,
    /// Env: `CRAWLER_MEDIA_QB_USER`.
    pub qb_user: Option<String>,
    /// Env: `CRAWLER_MEDIA_QB_PASS`.
    pub qb_pass: Option<String>,
    /// Env: `CRAWLER_MEDIA_QB_CATEGORY`.
    pub qb_category: Option<String>,
    /// Path mapping for qBittorrent (container → host).
    /// Env: `CRAWLER_MEDIA_QB_PATH_MAP`.
    pub qb_path_map_raw: Option<String>,

    // -- Transmission --
    /// Path mapping for Transmission.
    /// Env: `CRAWLER_MEDIA_TR_PATH_MAP`.
    pub tr_path_map_raw: Option<String>,

    /// Metadata proxy URL (http://... or socks5://...).
    /// Env: `CRAWLER_MEDIA_METADATA_PROXY`.
    pub metadata_proxy: Option<String>,
    /// Metadata proxy username.
    /// Env: `CRAWLER_MEDIA_METADATA_PROXY_USER`.
    pub metadata_proxy_user: Option<String>,
    /// Metadata proxy password.
    /// Env: `CRAWLER_MEDIA_METADATA_PROXY_PASS` (or `_FILE`).
    pub metadata_proxy_pass: Option<String>,
}

/// Hand-written so `debug!(?config)` cannot leak credentials. Secret fields are
/// rendered as `<redacted>`; a proxy URL that embeds credentials is masked.
impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn redacted(value: &Option<String>) -> Option<&'static str> {
            value.as_ref().map(|_| "<redacted>")
        }
        f.debug_struct("ServerConfig")
            .field("data_dir", &self.data_dir)
            .field("token", &redacted(&self.token))
            .field("admin_password", &redacted(&self.admin_password))
            .field("listen", &self.listen)
            .field("ui_dir", &self.ui_dir)
            .field("cors_origins", &self.cors_origins)
            .field("tmdb_key", &redacted(&self.tmdb_key))
            .field("tvdb_key", &redacted(&self.tvdb_key))
            .field("browser_enabled", &self.browser_enabled)
            .field("qb_url", &self.qb_url)
            .field("qb_user", &self.qb_user)
            .field("qb_pass", &redacted(&self.qb_pass))
            .field("qb_category", &self.qb_category)
            .field("qb_path_map_raw", &self.qb_path_map_raw)
            .field("tr_path_map_raw", &self.tr_path_map_raw)
            .field(
                "metadata_proxy",
                &self
                    .metadata_proxy
                    .as_deref()
                    .map(crate::http_agent::mask_proxy_url),
            )
            .field("metadata_proxy_user", &self.metadata_proxy_user)
            .field("metadata_proxy_pass", &redacted(&self.metadata_proxy_pass))
            .finish()
    }
}

impl ServerConfig {
    /// Parse all `CRAWLER_MEDIA_*` environment variables at startup.
    ///
    /// Unknown `CRAWLER_MEDIA_*` variables are logged as warnings to catch
    /// typos (e.g. `CRAWLER_MEDIA_TMDB_KET` instead of `_KEY`).
    pub fn from_env() -> Self {
        for key in unknown_env_keys(
            std::env::vars_os().filter_map(|(key, _)| key.into_string().ok()),
        ) {
            tracing::warn!(
                env = %key,
                "unrecognized CRAWLER_MEDIA_* environment variable (typo?)"
            );
        }

        Self {
            data_dir: data_dir_from_env(),
            token: secret("CRAWLER_MEDIA_TOKEN"),
            admin_password: secret("CRAWLER_MEDIA_ADMIN_PASSWORD"),
            listen: nonempty("CRAWLER_MEDIA_LISTEN").unwrap_or_else(|| "127.0.0.1:18765".into()),
            ui_dir: nonempty("CRAWLER_MEDIA_UI").map(PathBuf::from),
            cors_origins: cors_origins_from_env(),
            tmdb_key: secret("CRAWLER_MEDIA_TMDB_KEY"),
            tvdb_key: secret("CRAWLER_MEDIA_TVDB_KEY"),
            browser_enabled: env_flag("CRAWLER_MEDIA_BROWSER"),
            qb_url: nonempty("CRAWLER_MEDIA_QB_URL"),
            qb_user: nonempty("CRAWLER_MEDIA_QB_USER"),
            qb_pass: secret("CRAWLER_MEDIA_QB_PASS"),
            qb_category: nonempty("CRAWLER_MEDIA_QB_CATEGORY"),
            qb_path_map_raw: nonempty("CRAWLER_MEDIA_QB_PATH_MAP"),
            tr_path_map_raw: nonempty("CRAWLER_MEDIA_TR_PATH_MAP"),
            metadata_proxy: nonempty("CRAWLER_MEDIA_METADATA_PROXY"),
            metadata_proxy_user: nonempty("CRAWLER_MEDIA_METADATA_PROXY_USER"),
            metadata_proxy_pass: secret("CRAWLER_MEDIA_METADATA_PROXY_PASS"),
        }
    }
}

/// Environment lookup seam.
///
/// Production code reads the process environment via [`ProcessEnv`]; tests
/// inject a map instead, so they never mutate the process-global environment
/// (which would race with parallel tests).
pub trait EnvSource {
    /// Non-empty raw value of `name`, or `None`.
    fn value(&self, name: &str) -> Option<String>;
}

/// The real process environment.
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn value(&self, name: &str) -> Option<String> {
        nonempty(name)
    }
}

/// Resolve the data directory from `CRAWLER_MEDIA_DATA` (default `"data"`).
///
/// Shared by [`ServerConfig`] and by modules that need the path without a full
/// config instance, so the parsing rule lives in exactly one place.
pub fn data_dir_from_env() -> PathBuf {
    PathBuf::from(nonempty("CRAWLER_MEDIA_DATA").unwrap_or_else(|| "data".into()))
}

/// Parse the CORS allow-list from `CRAWLER_MEDIA_CORS_ORIGINS` (comma separated).
pub fn cors_origins_from_env() -> Vec<String> {
    nonempty("CRAWLER_MEDIA_CORS_ORIGINS")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default()
}

/// True when `CRAWLER_MEDIA_TMDB_KEY` (or its `_FILE` variant) supplies a key,
/// i.e. the value is pinned by the environment rather than the settings UI.
pub fn tmdb_key_from_env_is_set() -> bool {
    is_env_pinned(settings_keys::TMDB_API_KEY)
}

/// The single boolean vocabulary shared by env flags, settings KV flags and the
/// CLI: truthy `1`/`true`/`yes`/`on`, falsy `0`/`false`/`no`/`off`,
/// ASCII case-insensitive, surrounding whitespace ignored.
///
/// Returns `None` for empty or unknown input so each caller keeps its own
/// fallback (env/`KV` flags: off, CLI: an explicit error).
pub fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// [`parse_bool`] against an injected [`EnvSource`]; unknown or missing values
/// are off (a typo must not enable a feature).
pub fn env_flag_from(source: &dyn EnvSource, name: &str) -> bool {
    source
        .value(name)
        .and_then(|raw| parse_bool(&raw))
        .unwrap_or(false)
}

/// Proxy URL env fallbacks, in the precedence `http_agent` applies them.
///
/// These are *fallbacks*: a non-empty settings-KV value always wins. Only
/// `CRAWLER_MEDIA_METADATA_PROXY` is also seeded into the KV at startup, which
/// is why it — and not the generic `*_PROXY` variables — pins the setting.
const PROXY_URL_ENV_VARS: &[&str] = &[
    "CRAWLER_MEDIA_METADATA_PROXY",
    "ALL_PROXY",
    "all_proxy",
    "HTTP_PROXY",
    "http_proxy",
];

/// Proxy values supplied by the environment (non-empty; `_FILE` counted).
///
/// `url` is the only field that counts as an "env override" in the settings UI:
/// the store always wins for all three, so the credentials are plain fallbacks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyEnv {
    pub url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// Resolve proxy fallbacks from `source`. See [`PROXY_URL_ENV_VARS`].
pub fn proxy_env(source: &dyn EnvSource) -> ProxyEnv {
    ProxyEnv {
        url: PROXY_URL_ENV_VARS
            .iter()
            .find_map(|name| source.value(name)),
        username: secret_from(source, "CRAWLER_MEDIA_METADATA_PROXY_USER"),
        password: secret_from(source, "CRAWLER_MEDIA_METADATA_PROXY_PASS"),
    }
}

/// Settings keys whose value the environment overrides at startup, with the
/// env var that does it and whether its `_FILE` variant is honoured.
///
/// A pinned key is seeded from the environment on every start, so accepting a
/// settings-UI write would be a silent no-op after the next restart. The API
/// rejects such writes with `409 settings.env_pinned` instead.
pub fn env_pin(settings_key: &str) -> Option<(&'static str, bool)> {
    match settings_key {
        settings_keys::TMDB_API_KEY => Some(("CRAWLER_MEDIA_TMDB_KEY", true)),
        settings_keys::TVDB_API_KEY => Some(("CRAWLER_MEDIA_TVDB_KEY", true)),
        settings_keys::PROXY_METADATA => Some(("CRAWLER_MEDIA_METADATA_PROXY", false)),
        settings_keys::PROXY_USERNAME => Some(("CRAWLER_MEDIA_METADATA_PROXY_USER", false)),
        settings_keys::PROXY_PASSWORD => Some(("CRAWLER_MEDIA_METADATA_PROXY_PASS", true)),
        _ => None,
    }
}

/// True when the environment currently supplies a value for the pinning env var
/// of `settings_key` (non-empty; `_FILE` variants counted where supported).
pub fn is_env_pinned(settings_key: &str) -> bool {
    match env_pin(settings_key) {
        Some((name, true)) => secret(name).is_some(),
        Some((name, false)) => nonempty(name).is_some(),
        None => false,
    }
}

/// `409` message for a pinned key, or `None` when the key is writable.
pub fn env_pinned_message(settings_key: &str) -> Option<String> {
    let (name, _) = env_pin(settings_key)?;
    is_env_pinned(settings_key).then(|| {
        format!(
            "{name} 由环境变量固定，设置页的修改会在下次启动时被覆盖，因此已拒绝写入；\
             请移除该环境变量并重启服务后再修改"
        )
    })
}

/// `CRAWLER_MEDIA_*` names in `keys` that [`KNOWN_ENV_VARS`] does not list.
pub fn unknown_env_keys(keys: impl IntoIterator<Item = String>) -> Vec<String> {
    keys.into_iter()
        .filter(|key| {
            key.starts_with("CRAWLER_MEDIA_") && !KNOWN_ENV_VARS.contains(&key.as_str())
        })
        .collect()
}

fn nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Read a secret from env var `name`, falling back to the file path given by
/// `{name}_FILE`. Docker Swarm / Kubernetes secret volumes use this pattern so
/// credentials never appear in `docker inspect` or `/proc/<pid>/environ`.
pub fn secret(name: &str) -> Option<String> {
    secret_from(&ProcessEnv, name)
}

/// [`secret`] against an injected [`EnvSource`], so fallback paths that do not
/// own the process environment still honour `_FILE` variants.
pub fn secret_from(source: &dyn EnvSource, name: &str) -> Option<String> {
    if let Some(value) = source.value(name) {
        return Some(value);
    }
    let file_var = format!("{name}_FILE");
    let path = source.value(&file_var)?;
    read_secret_file(&path, &file_var)
}

/// Read and trim a secret-file's contents. Empty files yield `None` so a
/// mounted-but-empty secret does not silently override a real value.
fn read_secret_file(path: &str, var_name: &str) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let trimmed = content.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
        Err(err) => {
            tracing::warn!(env = %var_name, path, %err, "could not read secret file");
            None
        }
    }
}

fn env_flag(name: &str) -> bool {
    env_flag_from(&ProcessEnv, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_file_returns_trimmed_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "  s3cret\n").unwrap();
        assert_eq!(
            read_secret_file(path.to_str().unwrap(), "X_FILE"),
            Some("s3cret".into())
        );
    }

    #[test]
    fn empty_secret_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "\n  \n").unwrap();
        assert_eq!(read_secret_file(path.to_str().unwrap(), "X_FILE"), None);
    }

    #[test]
    fn missing_secret_file_is_none() {
        assert_eq!(
            read_secret_file("/nonexistent/definitely-missing", "X_FILE"),
            None
        );
    }
}
