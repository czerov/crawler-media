use std::fmt;
use std::str::FromStr;

use uuid::Uuid;

macro_rules! entity_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            pub fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }
    };
}

entity_id!(MediaId);
entity_id!(SiteId);
entity_id!(UserId);
entity_id!(SubscribeId);
entity_id!(FilterId);
entity_id!(LedgerId);
entity_id!(DownloaderId);
entity_id!(JobId);
entity_id!(JobDefId);
entity_id!(CollectionId);
entity_id!(LibraryId);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Movie,
    Tv,
    /// 「其他」库：不识别、不刮削的单本视频（文件名即标题）。
    Video,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Tv => "tv",
            Self::Video => "video",
        }
    }
}

impl FromStr for MediaKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "movie" => Ok(Self::Movie),
            "tv" => Ok(Self::Tv),
            "video" => Ok(Self::Video),
            other => Err(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Media {
    pub id: MediaId,
    pub kind: MediaKind,
    pub title: String,
    pub year: Option<u16>,
    pub original_title: Option<String>,
    /// 来自目录的其他译名。搜索前还会再过门槛，这里按详情顺序原样保存。
    pub aliases: Vec<String>,
    pub tmdb_id: Option<String>,
    pub douban_id: Option<String>,
    pub tvdb_id: Option<String>,
    pub bangumi_id: Option<String>,
    pub anilist_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserRole {
    Admin,
    Member,
}

impl UserRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }
}

impl FromStr for UserRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "admin" => Ok(Self::Admin),
            "member" => Ok(Self::Member),
            other => Err(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub id: UserId,
    pub login: String,
    pub enabled: bool,
    pub role: UserRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MaintenanceVia {
    #[default]
    Http,
    Browser,
}

impl MaintenanceVia {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Browser => "browser",
        }
    }
}

impl std::str::FromStr for MaintenanceVia {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "http" => Ok(Self::Http),
            "browser" => Ok(Self::Browser),
            other => Err(format!("unknown maintenance_via '{other}'")),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Site {
    pub id: SiteId,
    pub name: String,
    pub url: String,
    pub profile_id: String,
    pub cookie: Option<String>,
    pub api_key: Option<String>,
    pub rss_url: Option<String>,
    pub proxy: Option<String>,
    pub rate_limit_per_minute: Option<u32>,
    pub cdp_url: Option<String>,
    pub downloader_id: Option<DownloaderId>,
    pub enabled: bool,
    pub maintenance_via: MaintenanceVia,
}

/// 手写 `Debug`：`cookie` 是站点会话凭证、`api_key` 是站点 API 凭证，
/// 两者都绝不能进日志；`proxy` 可能内嵌 `user:pass@`，因此走全仓统一的
/// [`mask_proxy_url`]（保留用户名、掩码口令）。
///
/// 之前这里是 `#[derive(Debug)]`，任何一句 `debug!(?site)` 都会把凭证明文打进
/// 日志与前端"系统日志"。`Site` 本身没有 serde 派生（UI 载荷由 api 层单独
/// 构造），因此这里只影响 `Debug`。其余字段保留原样，便于诊断。
impl std::fmt::Debug for Site {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Site")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("url", &self.url)
            .field("profile_id", &self.profile_id)
            .field("cookie", &self.cookie.as_ref().map(|_| "<redacted>"))
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("rss_url", &self.rss_url)
            .field("proxy", &self.proxy.as_deref().map(mask_proxy_url))
            .field("rate_limit_per_minute", &self.rate_limit_per_minute)
            .field("cdp_url", &self.cdp_url)
            .field("downloader_id", &self.downloader_id)
            .field("enabled", &self.enabled)
            .field("maintenance_via", &self.maintenance_via)
            .finish()
    }
}

/// 代理 URL 脱敏渲染的**全仓唯一实现**。
///
/// `scheme://user:pass@host:port` → `scheme://user:***@host:port`：**保留用户名**
/// （排障时"哪个账号配错了"是有用信息）、只掩码口令，因为口令才是凭证。这与
/// api 侧既有调用点/断言（`ServerConfig.metadata_proxy`、`ProxyEnv.url`、
/// `http_agent` 的日志行、`proxy_settings` 的 `active_proxy`）完全一致，因此
/// 统一后 api 行为不变。
///
/// 规则：
/// - 只有用户名、没有口令的 userinfo 原样保留（没有可隐藏的秘密）；
/// - 没有 userinfo 的 URL 原样返回；
/// - path / query / fragment 里的 `@` 不会被误当成 userinfo（先按
///   `/`、`?`、`#` 切出 authority 再找 `@`）；
/// - 没有 scheme 的 `user:pass@host:port` 同样脱敏（旧 api 实现遇到这种输入
///   会原样返回，等于把口令写进日志）。
///
/// 放在 `domain`（最底层、不依赖任何上层 crate）是为了让 api 与 domain 共用
/// 同一份渲染；`domain` 不能反向依赖 api 的脱敏函数（违反 AGENTS.md 分层）。
pub fn mask_proxy_url(raw: &str) -> String {
    let (prefix, rest) = match raw.split_once("://") {
        Some((scheme, rest)) => (format!("{scheme}://"), rest),
        None => (String::new(), raw),
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    // host 不可能含 `@`，取最后一个 `@` 才能正确处理口令里未转义的 `@`。
    match authority.rsplit_once('@') {
        Some((userinfo, host)) => match userinfo.split_once(':') {
            Some((user, _password)) => format!("{prefix}{user}:***@{host}{tail}"),
            None => raw.to_string(),
        },
        None => raw.to_string(),
    }
}

#[cfg(test)]
mod debug_tests {
    use super::{Site, SiteId, mask_proxy_url};

    fn site_with_proxy(proxy: Option<&str>) -> Site {
        Site {
            id: SiteId::new(),
            name: "demo".into(),
            url: "https://pt.example/".into(),
            profile_id: "demo".into(),
            cookie: None,
            api_key: None,
            rss_url: None,
            proxy: proxy.map(str::to_string),
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: super::MaintenanceVia::Http,
        }
    }

    #[test]
    fn debug_never_prints_site_credentials() {
        let site = Site {
            id: SiteId::new(),
            name: "demo".into(),
            url: "https://pt.example/".into(),
            profile_id: "demo".into(),
            cookie: Some("uid=1; pass=cookie-secret".into()),
            api_key: Some("site-api-key-secret".into()),
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: super::MaintenanceVia::Http,
        };
        let rendered = format!("{site:?}");
        assert!(
            !rendered.contains("cookie-secret"),
            "Debug 不得输出 cookie: {rendered}"
        );
        assert!(
            !rendered.contains("site-api-key-secret"),
            "Debug 不得输出 api_key: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("pt.example"), "{rendered}");
        assert!(rendered.contains("demo"), "{rendered}");
    }

    /// 统一语义后：用户名保留（便于排障），口令必须消失。
    #[test]
    fn debug_masks_site_proxy_password_but_keeps_username() {
        let site = site_with_proxy(Some("http://bob:proxy-password-secret@127.0.0.1:7890"));
        let rendered = format!("{site:?}");
        assert!(
            !rendered.contains("proxy-password-secret"),
            "Debug 不得输出 proxy 口令: {rendered}"
        );
        assert!(
            rendered.contains("bob:***@127.0.0.1:7890"),
            "统一语义是保留用户名 + 掩码口令: {rendered}"
        );
    }

    #[test]
    fn mask_proxy_url_keeps_username_and_masks_password() {
        assert_eq!(
            mask_proxy_url("http://bob:pw@127.0.0.1:7890"),
            "http://bob:***@127.0.0.1:7890"
        );
        assert_eq!(
            mask_proxy_url("bob:pw@127.0.0.1:7890"),
            "bob:***@127.0.0.1:7890",
            "无 scheme 的 user:pass@host 同样要脱敏（旧 api 实现会原样返回）"
        );
        assert_eq!(
            mask_proxy_url("http://bob@127.0.0.1:7890"),
            "http://bob@127.0.0.1:7890",
            "只有用户名、没有口令时没有可隐藏的秘密，原样保留"
        );
        assert_eq!(
            mask_proxy_url("bob@127.0.0.1:7890"),
            "bob@127.0.0.1:7890"
        );
        assert_eq!(
            mask_proxy_url("socks5://127.0.0.1:1080"),
            "socks5://127.0.0.1:1080",
            "无 userinfo 时原样返回"
        );
        assert_eq!(
            mask_proxy_url("http://bob:pw@host:1/path@x"),
            "http://bob:***@host:1/path@x",
            "path 里的 @ 不是 userinfo"
        );
        assert_eq!(
            mask_proxy_url("http://host:1/path@x"),
            "http://host:1/path@x",
            "无 userinfo 但 path 含 @ 时不得误判"
        );
        assert_eq!(
            mask_proxy_url("http://bob:p@ss@host:1"),
            "http://bob:***@host:1",
            "口令里未转义的 @ 不能被当成 host 分隔符"
        );
        assert_eq!(mask_proxy_url(""), "");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchMode {
    Search,
    Rss,
    Both,
}

impl FetchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Rss => "rss",
            Self::Both => "both",
        }
    }
}

impl FromStr for FetchMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "search" => Ok(Self::Search),
            "rss" => Ok(Self::Rss),
            "both" => Ok(Self::Both),
            other => Err(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Coverage {
    Movie,
    Tv {
        season: u32,
        episode_from: u32,
        episode_to: Option<u32>,
    },
}

impl Coverage {
    /// Maximum bounded TV window accepted at the API boundary.
    pub const MAX_EPISODES: u32 = 1000;

    /// Validate a persisted Subscribe window before it can drive allocation or selection.
    pub fn validate(&self, full_season_pack: bool) -> Result<(), &'static str> {
        match self {
            Coverage::Movie if full_season_pack => Err("full_season_pack requires TV coverage"),
            Coverage::Movie => Ok(()),
            Coverage::Tv {
                season,
                episode_from,
                episode_to,
            } => {
                if *season == 0 || *episode_from == 0 {
                    return Err("TV season and first episode must be greater than zero");
                }
                if let Some(to) = episode_to {
                    if *to < *episode_from {
                        return Err("TV episode_to must be greater than or equal to episode_from");
                    }
                    if *to - *episode_from >= Self::MAX_EPISODES {
                        return Err("TV coverage cannot exceed 1000 episodes");
                    }
                } else if full_season_pack {
                    return Err("full_season_pack requires bounded TV coverage");
                }
                Ok(())
            }
        }
    }

    /// 覆盖范围内的全部单元 `(season, episode)`；movie = 单个 `(None, None)`。
    /// 与 subscribe_facts / subscribe_wanted 的键同构（-1 表示 movie）。
    pub fn units(&self) -> Vec<(Option<u32>, Option<u32>)> {
        match self {
            Coverage::Movie => vec![(None, None)],
            Coverage::Tv {
                season,
                episode_from,
                episode_to,
            } => {
                let to = episode_to.unwrap_or(*episode_from).max(*episode_from);
                let capped = episode_from.saturating_add(Self::MAX_EPISODES.saturating_sub(1));
                (*episode_from..=to.min(capped))
                    .map(|episode| (Some(*season), Some(episode)))
                    .collect()
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subscribe {
    pub id: SubscribeId,
    pub user_id: UserId,
    pub media_id: MediaId,
    pub coverage: Coverage,
    pub fetch_mode: FetchMode,
    pub filter_id: FilterId,
    pub wash_cut: bool,
    pub wash_cut_filter_id: Option<FilterId>,
    /// 洗版时保留被替换的旧版本（同时留 1080p 与 4K）；默认替换即删。
    pub keep_old_versions: bool,
    pub full_season_pack: bool,
    pub downloader_id: Option<DownloaderId>,
    /// Target library id to transfer/organize media into. If None, falls back to default library or match rules.
    pub library_id: Option<LibraryId>,
    /// "active" | "paused" — persisted tracking state.
    pub tracking_state: String,
    /// Whether the coverage window extends as future episodes air.
    pub follow_future: bool,
    /// Seconds between SubscribeSearch runs. Default 1800 (30 minutes).
    pub search_interval_secs: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    pub id: FilterId,
    pub name: String,
    pub atoms: Vec<FilterAtom>,
    pub keep_old_versions: bool,
}

impl Filter {
    pub fn new(id: FilterId, name: impl Into<String>, atoms: Vec<FilterAtom>) -> Self {
        Self {
            id,
            name: name.into(),
            atoms,
            keep_old_versions: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterAtom {
    pub priority: i32,
    pub rule: AtomRule,
    /// exclude 原子命中即排除（黑名单语义：平台/制作组/HDR 黑名单）。
    pub exclude: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AtomRule {
    Resolution(String),
    Source(String),
    Free,
    Hr,
    Codec(String),
    TitleMatch(String),
    /// HDR 格式（Release.hdr，如 `HDR10` / `DV`；区别于 `Hr` 的考核标志）。
    Hdr(String),
    /// 体积区间（MiB，按单集均摊比较：整季包用总体积 ÷ 集数）。
    Size {
        min_mb: Option<u64>,
        max_mb: Option<u64>,
    },
    /// 做种数下限。
    MinSeeders(u32),
    /// 要求的字幕语言（规范化 BCP 47，如 `zh` / `zh-Hans` / `en`）。
    SubtitleLanguage(String),
    /// 要求的音轨语言（规范化 BCP 47，如 `cmn` / `yue` / `en`）。
    AudioLanguage(String),
    /// 站点白名单（SiteId）。
    Site(String),
    /// 洗版目标档位（如 `2160p Remux`）：不参与 admit 打分，只被 wash-target 读取。
    WashTarget(String),
    /// 洗版比较维度顺序（逗号分隔，如 `resolution,source`）：配置后替换判定
    /// 从「分数更高」改为按维度逐项比较新老版本。
    UpgradeLadder(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QualitySource {
    Probe,
    Release,
}

impl QualitySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Probe => "probe",
            Self::Release => "release",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confidence {
    High,
    Low,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Low => "low",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerRow {
    pub id: LedgerId,
    pub media_id: MediaId,
    pub path: String,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub resolution: Option<String>,
    pub codec: Option<String>,
    pub hdr: Option<String>,
    pub quality_source: QualitySource,
    pub confidence: Confidence,
    pub filter_score: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Torrent {
    pub site_id: SiteId,
    pub title: String,
    pub enclosure: String,
    pub size_bytes: Option<u64>,
    pub seeders: Option<u32>,
    pub free: bool,
    pub hr: bool,
    pub imdb_id: Option<String>,
    /// 站点内种子 id（详情/下载链接提取；RSS 用 guid）。
    pub id: Option<String>,
    pub leechers: Option<u32>,
    pub snatched: Option<u32>,
    /// 发布/上传时间（站点原文或 RFC3339）。
    pub upload_time: Option<String>,
    pub detail_url: Option<String>,
    pub category: Option<String>,
    pub poster_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub title: String,
    pub year: Option<u16>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub episode_to: Option<u32>,
    pub resolution: Option<String>,
    pub source: Option<String>,
    pub codec: Option<String>,
    pub hdr: Option<String>,
    pub subtitle_language: Option<String>,
    pub audio_language: Option<String>,
    pub group: Option<String>,
    pub confidence: Confidence,
}

impl Release {
    /// Inclusive episode span after clamping inverted / oversized ranges.
    /// Callers must not iterate `episode..=episode_to` themselves.
    pub fn episode_span(&self) -> Option<(u32, u32)> {
        let from = self.episode?;
        let to = self.episode_to.unwrap_or(from).max(from);
        let capped = from.saturating_add(Coverage::MAX_EPISODES.saturating_sub(1));
        Some((from, to.min(capped)))
    }

    /// Bounded TV units this Release covers. Movie / unknown season stays empty.
    pub fn covered_episodes(&self) -> Vec<(u32, u32)> {
        let Some(season) = self.season else {
            return Vec::new();
        };
        let Some((from, to)) = self.episode_span() else {
            return Vec::new();
        };
        (from..=to).map(|episode| (season, episode)).collect()
    }

    /// Episode count used for size amortization and similar per-episode math.
    pub fn episode_count(&self) -> u32 {
        self.episode_span()
            .map(|(from, to)| to.saturating_sub(from) + 1)
            .unwrap_or(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tv_range(from: u32, to: u32) -> Release {
        Release {
            title: "Show".into(),
            year: None,
            season: Some(1),
            episode: Some(from),
            episode_to: Some(to),
            resolution: None,
            source: None,
            codec: None,
            hdr: None,
            subtitle_language: None,
            audio_language: None,
            group: None,
            confidence: Confidence::High,
        }
    }

    #[test]
    fn huge_release_range_is_capped_without_enumerating_u32_max() {
        let release = tv_range(1, u32::MAX);
        let (from, to) = release.episode_span().expect("range");
        assert_eq!(from, 1);
        assert_eq!(to, Coverage::MAX_EPISODES);
        let units = release.covered_episodes();
        assert_eq!(units.len(), Coverage::MAX_EPISODES as usize);
        assert_eq!(units[0], (1, 1));
        assert_eq!(units[units.len() - 1], (1, Coverage::MAX_EPISODES));
        assert_eq!(release.episode_count(), Coverage::MAX_EPISODES);
    }
}
