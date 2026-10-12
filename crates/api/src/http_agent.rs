use std::sync::LazyLock;
use std::time::Duration;

use parking_lot::RwLock;
use ureq::config::IpFamily;

use crate::Store;
use crate::settings_keys;

static CONFIGURED_PROXY: RwLock<Option<String>> = RwLock::new(None);
static DOUBAN_BYPASS: RwLock<bool> = RwLock::new(true);
static CUSTOM_ALLOWED_DOMAINS: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// Build a fully qualified proxy URL, attaching username and password if provided separately.
/// If `raw_url` already embeds credentials (`user:pass@host`), it is returned as is.
pub fn format_proxy_url(raw_url: &str, username: Option<&str>, password: Option<&str>) -> String {
    let raw = raw_url.trim();
    if raw.is_empty() {
        return String::new();
    }
    if raw.contains('@') {
        return raw.to_string();
    }
    let Some(user) = username.map(str::trim).filter(|u| !u.is_empty()) else {
        return raw.to_string();
    };

    fn percent_encode(s: &str) -> String {
        let mut out = String::new();
        for b in s.bytes() {
            match b {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char);
                }
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }

    let user_enc = percent_encode(user);
    let creds = match password.map(str::trim).filter(|p| !p.is_empty()) {
        Some(pass) => format!("{user_enc}:{}", percent_encode(pass)),
        None => user_enc,
    };

    if let Some((scheme, rest)) = raw.split_once("://") {
        format!("{scheme}://{creds}@{rest}")
    } else {
        format!("{creds}@{raw}")
    }
}

/// Sync proxy configuration from Store settings into memory for hot reloading.
///
/// Store values win; the environment only fills the gaps. The env side uses the
/// same parser as [`crate::config::ServerConfig`], so `_FILE` secret variants
/// resolve here too.
pub fn sync_from_store(store: &Store) {
    let env = crate::config::proxy_env(&crate::config::ProcessEnv);
    let raw_proxy = store
        .get_setting(settings_keys::PROXY_METADATA)
        .ok()
        .flatten()
        .filter(|p| !p.trim().is_empty())
        .or(env.url);

    let username = store
        .get_setting(settings_keys::PROXY_USERNAME)
        .ok()
        .flatten()
        .filter(|u| !u.trim().is_empty())
        .or(env.username);

    let password = store
        .get_setting(settings_keys::PROXY_PASSWORD)
        .ok()
        .flatten()
        .filter(|p| !p.trim().is_empty())
        .or(env.password);

    let proxy =
        raw_proxy.map(|url| format_proxy_url(&url, username.as_deref(), password.as_deref()));
    *CONFIGURED_PROXY.write() = proxy;

    let bypass = store
        .get_setting(settings_keys::PROXY_DOUBAN_BYPASS)
        .ok()
        .flatten()
        .and_then(|v| crate::config::parse_bool(&v))
        .unwrap_or(true);
    *DOUBAN_BYPASS.write() = bypass;

    let custom_domains = store
        .get_setting(settings_keys::PROXY_ALLOWED_DOMAINS)
        .ok()
        .flatten()
        .map(|raw| {
            raw.split(|c| c == ',' || c == '\n' || c == ';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_ascii_lowercase())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    *CUSTOM_ALLOWED_DOMAINS.write() = custom_domains;
}

pub fn current_proxy() -> Option<String> {
    CONFIGURED_PROXY.read().clone()
}

pub fn custom_allowed_domains() -> Vec<String> {
    CUSTOM_ALLOWED_DOMAINS.read().clone()
}

pub fn douban_bypass() -> bool {
    *DOUBAN_BYPASS.read()
}

pub fn build_agent(proxy_url: Option<&str>, family: IpFamily) -> ureq::Agent {
    let mut builder = ureq::Agent::config_builder()
        .ip_family(family)
        .user_agent(crate::user_agent::active_user_agent())
        .timeout_resolve(Some(Duration::from_secs(3)))
        .timeout_connect(Some(Duration::from_secs(4)))
        .timeout_global(Some(Duration::from_secs(6)));

    if let Some(url) = proxy_url.filter(|u| !u.trim().is_empty()) {
        let trimmed = url.trim();
        // 关键优化：对于 socks5://，自动转换为 socks5h://
        // 这样 SOCKS5 代理会在远端解析域名，彻底解决本地 DNS 污染（如 api.bgm.tv 被国内 DNS 投毒到 Facebook IP 导致证书不匹配）
        let remote_dns_url = if trimmed.starts_with("socks5://") {
            format!("socks5h://{}", &trimmed["socks5://".len()..])
        } else {
            trimmed.to_string()
        };

        if let Ok(proxy) = ureq::Proxy::new(&remote_dns_url) {
            builder = builder.proxy(Some(proxy));
        } else if let Ok(proxy) = ureq::Proxy::new(trimmed) {
            builder = builder.proxy(Some(proxy));
        }
    }
    builder.build().new_agent()
}

static DIRECT_V4: LazyLock<RwLock<ureq::Agent>> =
    LazyLock::new(|| RwLock::new(build_agent(None, IpFamily::Ipv4Only)));
static DIRECT_V6: LazyLock<RwLock<ureq::Agent>> =
    LazyLock::new(|| RwLock::new(build_agent(None, IpFamily::Ipv6Only)));

pub fn refresh_direct_agents() {
    *DIRECT_V4.write() = build_agent(None, IpFamily::Ipv4Only);
    *DIRECT_V6.write() = build_agent(None, IpFamily::Ipv6Only);
}

/// 允许走全局代理的外部元数据域名白名单。
/// 只有属于该列表的国外服务才允许通过代理转发；网盘、视频直链、测试用例等一律直连。
const PROXY_ALLOWED_HOST_SUFFIXES: &[&str] = &[
    "themoviedb.org",
    "tmdb.org",
    "thetvdb.com",
    "anilist.co",
    "bgm.tv",
    "theintrodb.org",
];

/// 检查给定 URL 是否应该走配置的代理
pub fn should_use_proxy_for_url(url: &str) -> bool {
    let host = extract_host_from_url(url);
    if host.is_empty() {
        return false;
    }
    // 明确忽略测试域名、内网与本地
    if host == "localhost"
        || host == "127.0.0.1"
        || host.ends_with(".example.com")
        || host == "example.com"
    {
        return false;
    }
    // 豆瓣默认强制直连（绕过代理）
    if host.ends_with("douban.com") || host.ends_with("doubanio.com") {
        return !douban_bypass();
    }
    // 检查用户自定义配置的代理域名列表
    let custom = CUSTOM_ALLOWED_DOMAINS.read();
    if custom
        .iter()
        .any(|domain| host.ends_with(domain) || host == *domain)
    {
        return true;
    }
    // 内置白名单域名才允许走代理
    PROXY_ALLOWED_HOST_SUFFIXES
        .iter()
        .any(|suffix| host.ends_with(suffix))
}

fn extract_host_from_url(url: &str) -> String {
    let without_scheme = if let Some((_, rest)) = url.split_once("://") {
        rest
    } else {
        url
    };
    let host_and_port = without_scheme
        .split(&['/', '?', '#'][..])
        .next()
        .unwrap_or_default();
    let host = host_and_port.split(':').next().unwrap_or_default();
    host.to_ascii_lowercase()
}

/// Run an outbound request for a specific target URL, automatically checking the whitelist.
/// Only whitelisted domains use the configured proxy; all others direct play.
pub fn call_url<T>(
    url: &str,
    build: impl Fn(&ureq::Agent) -> Result<T, ureq::Error>,
) -> Result<T, ureq::Error> {
    if should_use_proxy_for_url(url) {
        call(build)
    } else {
        call_with_proxy(None, build)
    }
}

/// Run one outbound catalog request over IPv4, then IPv6, using the globally configured proxy.
pub fn call<T>(build: impl Fn(&ureq::Agent) -> Result<T, ureq::Error>) -> Result<T, ureq::Error> {
    let proxy = current_proxy();
    call_with_proxy(proxy.as_deref(), build)
}

/// Call for Douban, respecting the `douban_bypass` setting (default true).
pub fn call_douban<T>(
    build: impl Fn(&ureq::Agent) -> Result<T, ureq::Error>,
) -> Result<T, ureq::Error> {
    if douban_bypass() {
        call_with_proxy(None, build)
    } else {
        call(build)
    }
}

/// Mask password in a proxy url for safe logging.
pub fn mask_proxy_url(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        if let Some((creds, host)) = rest.split_once('@') {
            if let Some((user, _pass)) = creds.split_once(':') {
                return format!("{scheme}://{user}:***@{host}");
            }
            return format!("{scheme}://{creds}@{host}");
        }
    }
    url.to_string()
}

/// Run an outbound request with an explicitly specified proxy (or None for direct).
pub fn call_with_proxy<T>(
    proxy: Option<&str>,
    build: impl Fn(&ureq::Agent) -> Result<T, ureq::Error>,
) -> Result<T, ureq::Error> {
    if let Some(proxy_url) = proxy.filter(|p| !p.trim().is_empty()) {
        tracing::info!(
            proxy = %mask_proxy_url(proxy_url),
            "出站元数据请求已通过代理转发"
        );
        let v4 = build_agent(Some(proxy_url), IpFamily::Ipv4Only);
        let v6 = build_agent(Some(proxy_url), IpFamily::Ipv6Only);
        match build(&v4) {
            Ok(res) => Ok(res),
            Err(v4_err) => build(&v6).map_err(|_| v4_err),
        }
    } else {
        tracing::debug!("出站元数据请求直连（无代理）");
        let v4 = DIRECT_V4.read().clone();
        let v6 = DIRECT_V6.read().clone();
        match build(&v4) {
            Ok(res) => Ok(res),
            Err(v4_err) => build(&v6).map_err(|_| v4_err),
        }
    }
}
