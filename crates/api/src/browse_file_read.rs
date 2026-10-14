//! 浏览路径的只读文件访问 seam。
//!
//! NFO 读取与文件 stat 必须在 Store 锁之外执行：它们可能落在网络存储上，
//! 持锁做这些 IO 会把整个进程的数据库访问一起拖住。测试注入一个会阻塞的
//! 实现，用来证明这些调用确实不持有 Store 锁。

use std::path::Path;

/// 浏览所需的只读文件访问。
pub trait BrowseFileRead: Send + Sync {
    /// 读取并解析 NFO；文件缺失或无效时返回 `None`。
    fn read_nfo(&self, path: &Path) -> Option<library::NfoMeta>;

    /// 文件元数据（排序用的 added_at / size）。
    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata>;

    /// 普通文件存在性判断（海报 / 背景 / 剧照探测）。
    ///
    /// 投影构建与浏览回退路径都走这里，测试可注入计数实现。
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    /// 发现视频旁的 external 字幕轨（文件系统 IO）。
    ///
    /// 默认实现保持既有 implementor source-compatible；需要阻塞注入的测试
    /// 实现可覆盖它。
    fn external_subtitles(&self, path: &Path) -> Result<Vec<library::SubtitleTrack>, String> {
        library::external_subtitle_tracks(path).map_err(|error| error.to_string())
    }
}

/// 生产实现：直接访问磁盘。
pub struct LocalBrowseFileRead;

impl BrowseFileRead for LocalBrowseFileRead {
    fn read_nfo(&self, path: &Path) -> Option<library::NfoMeta> {
        library::read_nfo(path)
    }

    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }
}
