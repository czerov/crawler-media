use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    SubscribeSearch,
    SubscribeRss,
    Transfer,
    Scrape,
    CheckIn,
    CatalogRefresh,
    WatchIntake,
    /// 数据库维护（optimize / 受控 VACUUM）。见 DB8 方案。
    DatabaseMaintenance,
    /// 终止态任务历史清理。
    JobsRetention,
    /// 播放历史清理。
    PlaybackHistoryRetention,
    /// 浏览器生命周期对账。
    BrowserReconcile,
    /// 分类历史保留：QoE、transient catalog cache、probe 终态。
    DataRetention,
}

impl JobKind {
    /// 维护类任务：由 `api::maintenance` 提供策略与执行器，不参与订阅/下载路径。
    pub fn is_maintenance(self) -> bool {
        matches!(
            self,
            Self::DatabaseMaintenance
                | Self::JobsRetention
                | Self::PlaybackHistoryRetention
                | Self::BrowserReconcile
                | Self::DataRetention
        )
    }
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SubscribeSearch => "subscribe_search",
            Self::SubscribeRss => "subscribe_rss",
            Self::Transfer => "transfer",
            Self::Scrape => "scrape",
            Self::CheckIn => "check_in",
            Self::CatalogRefresh => "catalog_refresh",
            Self::WatchIntake => "watch_intake",
            Self::DatabaseMaintenance => "database_maintenance",
            Self::JobsRetention => "jobs_retention",
            Self::PlaybackHistoryRetention => "playback_history_retention",
            Self::BrowserReconcile => "browser_reconcile",
            Self::DataRetention => "data_retention",
        }
    }
}

impl FromStr for JobKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "subscribe_search" => Ok(Self::SubscribeSearch),
            "subscribe_rss" => Ok(Self::SubscribeRss),
            "transfer" => Ok(Self::Transfer),
            "scrape" => Ok(Self::Scrape),
            "check_in" => Ok(Self::CheckIn),
            "catalog_refresh" => Ok(Self::CatalogRefresh),
            "watch_intake" => Ok(Self::WatchIntake),
            "database_maintenance" => Ok(Self::DatabaseMaintenance),
            "jobs_retention" => Ok(Self::JobsRetention),
            "playback_history_retention" => Ok(Self::PlaybackHistoryRetention),
            "browser_reconcile" => Ok(Self::BrowserReconcile),
            "data_retention" => Ok(Self::DataRetention),
            other => Err(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Schedule {
    Interval { secs: u64 },
}

impl Schedule {
    pub fn as_str(&self) -> String {
        match self {
            Self::Interval { secs } => format!("interval:{secs}"),
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        let Some(secs) = raw.strip_prefix("interval:") else {
            return Err(raw.to_string());
        };
        let secs = secs.parse::<u64>().map_err(|_| raw.to_string())?;
        Ok(Self::Interval { secs })
    }

    pub fn next_after(&self, now: i64) -> i64 {
        match self {
            // saturating：超大 secs（u64）在 release 下 `now + i64::MAX` 会回绕成负数，
            // 使 run_after <= now → 立即被 claim → follow-up 再算一次负数 → 每 tick
            // 反复执行、永不停止（DB8 复核 I2 后果 C）。
            Self::Interval { secs } => now.saturating_add(i64::try_from(*secs).unwrap_or(i64::MAX)),
        }
    }
}
