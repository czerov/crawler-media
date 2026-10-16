//! Centralized settings KV key constants.
//!
//! All keys used in the `settings` table should be defined here to prevent
//! typos and enable IDE navigation. Use these constants instead of string
//! literals throughout the codebase.

/// TMDB API key.
pub const TMDB_API_KEY: &str = "tmdb.api_key";
/// TVDB API key.
pub const TVDB_API_KEY: &str = "tvdb.api_key";
/// Default filter id.
pub const DEFAULT_FILTER_ID: &str = "default_filter_id";
/// Transfer mode (hardlink / copy / move).
pub const TRANSFER_MODE: &str = "transfer_mode";
/// Metadata language preference (e.g. "zh-CN").
pub const METADATA_LANGUAGE: &str = "metadata.language";
/// Scrape configuration JSON.
pub const METADATA_SCRAPE: &str = "metadata.scrape";
/// Revoked playback devices JSON.
pub const PLAYBACK_REVOKED_DEVICES: &str = "playback.revoked_devices";
/// Metadata proxy URL (http://... or socks5://...).
pub const PROXY_METADATA: &str = "proxy.metadata";
/// Optional proxy username.
pub const PROXY_USERNAME: &str = "proxy.username";
/// Optional proxy password.
pub const PROXY_PASSWORD: &str = "proxy.password";
/// Whether Douban should bypass proxy (default: true).
pub const PROXY_DOUBAN_BYPASS: &str = "proxy.douban_bypass";
/// Comma or newline-separated custom allowed domains for proxy.
pub const PROXY_ALLOWED_DOMAINS: &str = "proxy.allowed_domains";
/// CDP enabled flag ("1" or "0").
pub const CDP_ENABLED: &str = "cdp.enabled";
/// CDP remote debugging URL (e.g. "http://127.0.0.1:9222").
pub const CDP_URL: &str = "cdp.url";
/// Obscura install policy ("preinstalled" or "download").
pub const OBSCURA_INSTALL: &str = "obscura.install";
/// 媒体信息探测 worker 并发数（默认 1；上限 8）。声纹使用独立的单 worker 队列。
pub const PROBE_CONCURRENCY: &str = "probe.concurrency";
/// 系统全局统一 User-Agent（默认 crawler-media/0.1.0）。
pub const GLOBAL_USER_AGENT: &str = "network.user_agent";
