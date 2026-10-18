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
/// CDP cookie sync enabled flag ("1" or "0").
pub const CDP_SYNC_ENABLED: &str = "cdp.sync.enabled";
/// CDP remote debugging URL (e.g. "http://127.0.0.1:9222").
pub const CDP_URL: &str = "cdp.url";
/// Obscura browser anti-detect enabled flag ("1" or "0").
pub const OBSCURA_ENABLED: &str = "obscura.enabled";
/// Obscura CDP / endpoint URL (e.g. "ws://127.0.0.1:9223" or "http://127.0.0.1:9223").
pub const OBSCURA_URL: &str = "obscura.url";
/// 媒体信息探测 worker 并发数（默认 1；上限 8）。声纹使用独立的单 worker 队列。
pub const PROBE_CONCURRENCY: &str = "probe.concurrency";
/// 系统全局统一 User-Agent（默认 crawler-media/0.1.0）。
pub const GLOBAL_USER_AGENT: &str = "network.user_agent";
/// G-H1 air-date gate for scheduled TV searches ("0"/"false" disables; default on).
pub const SUBSCRIBE_AIR_DATE_GATE: &str = "subscribe.air_date_gate";
/// M1 overlap policy when a range pack and single episodes cover the same slots:
/// `prefer_pack` (default) or `prefer_episode`.
pub const SUBSCRIBE_OVERLAP_POLICY: &str = "subscribe.overlap_policy";
/// F-1: download only the files of missing episodes when a pack fills a gap.
pub const SUBSCRIBE_SELECTIVE_FILES: &str = "subscribe.selective_files";
/// Review N-1: rounds a wash-cut arrival may wait for a source probe before
/// it is rejected (default 6; the wait is also capped at 24 hours).
pub const SUBSCRIBE_PROBE_WAIT_MAX_ATTEMPTS: &str = "subscribe.probe_wait_max_attempts";
/// D-M2: `copy` (default) or `fail` when a hardlink transfer cannot link.
pub const LIBRARY_HARDLINK_FALLBACK: &str = "library.hardlink_fallback";
/// G-M5: "1"/"true" allows several instances on one data directory (unsafe).
pub const INSTANCE_ALLOW_MULTI: &str = "instance.allow_multi";
/// H3: days without download progress before a pending is marked stalled (default 7).
pub const SUBSCRIBE_STALLED_TIMEOUT_DAYS: &str = "subscribe.stalled_timeout_days";
