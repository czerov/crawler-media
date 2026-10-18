use std::path::Path;

use domain::{Filter, LedgerRow, Media, Subscribe, Torrent};
use downloader::Downloader;
use hooks::{Bus, PluginError};
use library::TransferMode;

use crate::facts::SubscribeFacts;

#[derive(Debug, thiserror::Error)]
pub enum SubscribeError {
    #[error(transparent)]
    Downloader(#[from] downloader::DownloaderError),
    #[error(transparent)]
    Library(#[from] library::LibraryError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Hook(#[from] PluginError),
}

pub struct RunInput<'a, D: Downloader + ?Sized> {
    pub subscribe: &'a Subscribe,
    pub media: &'a Media,
    pub filter: &'a Filter,
    /// 洗版目标规则组（wash_cut_filter_id 对应；None = 复用 filter）。
    /// choose 读它的 UpgradeLadder / WashTarget 原子决定替换判定。
    pub wash_filter: Option<&'a Filter>,
    pub torrents: Vec<Torrent>,
    /// 搜索候选时使用的关键词（原语种 + 显示语种）。命中关键词即视为
    /// 与 Media 相关——中英文标题无需互相包含也能匹配。
    pub search_keywords: Vec<String>,
    pub facts: SubscribeFacts,
    pub downloader: &'a D,
    pub library_root: &'a Path,
    pub transfer_mode: Option<TransferMode>,
    pub scrape: bool,
    pub hooks: Option<&'a Bus>,
    /// 入库命名模板（该 Media 类型的完整合成模式）；None = 内置默认。
    pub naming: Option<&'a str>,
    /// 洗版替换时保留被替换文件（不删除），交由调用方决定去处（回收站）。
    pub preserve_removed: bool,
    /// M1: how overlapping range packs and single episodes are resolved.
    pub overlap: crate::choose::OverlapPolicy,
    /// G-H2b: catalog season lengths for mapping absolute episode numbers.
    pub absolute: Option<&'a crate::AbsoluteEpisodes>,
}

pub struct RunOutcome {
    pub facts: SubscribeFacts,
    pub ledger: Vec<LedgerRow>,
    /// Original completed file for each ledger path written during this run.
    /// The API persists it so an administrator can explicitly retransfer a
    /// missing Library file without re-running the subscription workflow.
    pub ledger_sources: Vec<LedgerSource>,
    pub removed_paths: Vec<String>,
    pub completed: bool,
    /// Enclosures whose completed files were transferred into the Library.
    pub transferred_enclosures: Vec<String>,
    /// Wash-cut arrivals whose actual (probed) quality vetoed the replacement
    /// (H2). Persisted as `rejected` so automatic rounds never pick them again.
    pub rejected_enclosures: Vec<String>,
    /// Review N-1: wash-cut arrivals whose source could not be probed yet;
    /// nothing was published. The API caps how long they may wait.
    pub awaiting_probe_enclosures: Vec<String>,
    /// Failures that occurred while collecting a multi-file download. Successful
    /// files remain in the outcome so callers can persist them before retrying.
    pub collection_errors: Vec<String>,
    /// Failures submitting selected torrents to the Downloader.
    pub submission_errors: Vec<String>,
    /// All torrents added to downloader during this run along with their scores.
    pub torrents_added: Vec<(i32, Torrent)>,
}

pub struct LedgerSource {
    pub ledger_path: String,
    pub source_path: String,
}
