mod http;
mod identity;
mod owned;
pub use owned::{magnet_info_hash, ownership_tag, unproven};
mod memory;
mod path_map;
mod pool;
mod qbit;
mod torrent_file;
mod transmission;
pub use torrent_file::torrent_info_hash;

use std::path::PathBuf;
use std::sync::Arc;

use domain::Torrent;

pub(crate) use http::agent_for_url;
pub use identity::{
    core_titles_compatible, has_sample_mismatch, release_units_agree, torrent_matches_snapshot,
};
pub use memory::MemoryDownloader;
pub use path_map::{PathMap, apply_maps};
pub use pool::DownloaderSet;
pub use qbit::{QbitConfig, QbitDownloader};
pub use transmission::{TransmissionConfig, TransmissionDownloader};

#[derive(Debug, thiserror::Error)]
pub enum DownloaderError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Message(String),
    /// G-M2: an owned task for this enclosure exists but is unusable
    /// (`error` / `missingFiles`); callers must switch source.
    #[error("existing downloader task {0} is broken")]
    ExistingTaskBroken(String),
}

pub trait Downloader: Send + Sync {
    fn add(&self, torrent: &Torrent) -> Result<(), DownloaderError>;
    /// D-M3: re-teach a client the info hash persisted for `torrent` (e.g.
    /// after a restart). Clients that identify tasks by tags ignore it.
    fn remember_identity(&self, _torrent: &Torrent, _hash: &str) -> Result<(), DownloaderError> {
        Ok(())
    }
    /// Add a torrent to an explicit downloader-side directory. Existing
    /// implementations remain source-compatible; unsupported clients reject
    /// an explicit directory instead of silently ignoring the user's choice.
    fn add_with_options(
        &self,
        torrent: &Torrent,
        download_url: &str,
        save_path: Option<&str>,
    ) -> Result<(), DownloaderError> {
        if save_path.is_some() {
            return Err(DownloaderError::Message(
                "custom save paths are not supported by this downloader".into(),
            ));
        }
        if download_url == torrent.enclosure {
            self.add(torrent)
        } else {
            self.add_resolved(torrent, download_url)
        }
    }
    /// Submit with a short-lived download URL while retaining the Torrent's
    /// stable enclosure for matching, tags, and pending identity.
    fn add_resolved(&self, torrent: &Torrent, download_url: &str) -> Result<(), DownloaderError> {
        let mut resolved = torrent.clone();
        resolved.enclosure = download_url.to_string();
        self.add(&resolved)
    }
    fn completed_files(&self, torrent: &Torrent) -> Result<Vec<PathBuf>, DownloaderError>;

    /// Review H-3: whether the task for `torrent` is a claimed user task.
    fn is_claimed(&self, _torrent: &Torrent) -> Result<bool, DownloaderError> {
        Ok(false)
    }

    /// D-L1: files finished so far (even while other files of the task are
    /// still downloading) and whether the task's wanted files are all done.
    /// The default keeps the whole-task behaviour of `completed_files`.
    fn collectable_files(
        &self,
        torrent: &Torrent,
    ) -> Result<(Vec<PathBuf>, bool), DownloaderError> {
        let files = self.completed_files(torrent)?;
        let done = !files.is_empty();
        Ok((files, done))
    }

    /// Uploaded bytes for torrents in a category (boost/刷流 stats).
    /// Downloaders without upload telemetry return 0.
    fn uploaded_by_category(&self, _category: &str) -> Result<u64, DownloaderError> {
        Ok(0)
    }

    /// 从客户端删除该种子的下载任务（季清理 / 退订联动）。
    /// `delete_files: false` 保留做种文件——Library 侧清理由调用方负责
    /// （wash-cut 语义：删除旧 Library 文件不影响下载器继续做种）。
    /// 默认不支持；qBittorrent / Transmission 已实现。
    fn remove(&self, _torrent: &Torrent, _delete_files: bool) -> Result<(), DownloaderError> {
        Err(DownloaderError::Message("remove not supported".into()))
    }

    /// Destructive Subscribe cleanup must prove exact identity. Never falls back
    /// to `remove`, whose legacy implementations may match names and sizes.
    fn remove_owned(&self, _torrent: &Torrent, _delete_files: bool) -> Result<(), DownloaderError> {
        Err(DownloaderError::Message(
            "exact owned removal unsupported; refusing unsafe cleanup".into(),
        ))
    }

    /// F-1: mark files for which `keep(name)` is false as not-to-download.
    /// Returns how many files were skipped, or `None` when the task or its
    /// file list (magnet metadata) is not known yet, or the client cannot
    /// select files.
    fn select_files(
        &self,
        _torrent: &Torrent,
        _keep: &dyn Fn(&str) -> bool,
    ) -> Result<Option<usize>, DownloaderError> {
        Ok(None)
    }

    /// Canonical actual task identity, not a title-derived snapshot match.
    /// None means ownership cannot be proven (including legacy HTTP deliveries).
    fn owned_identity(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        Ok(magnet_info_hash(&torrent.enclosure))
    }

    /// The endpoint this connected client actually talks to, as
    /// `"<kind>|<normalized url>"`. Ownership routing compares this instead of
    /// re-deriving configuration, so a runtime override (including secrets
    /// resolved from files) is reflected exactly. None means "cannot report".
    fn endpoint(&self) -> Option<String> {
        None
    }

    /// If this value is a runtime proxy, return the concrete client selected
    /// right now. Cleanup freezes that snapshot so a later configuration
    /// change cannot retarget a planned delete. Concrete clients return None.
    fn snapshot_client(&self) -> Result<Option<Arc<dyn Downloader>>, DownloaderError> {
        Ok(None)
    }

    /// Delete a task by info hash. `delete_files`: remove downloaded data too.
    fn delete_task(&self, _info_hash: &str, _delete_files: bool) -> Result<(), DownloaderError> {
        Err(DownloaderError::Message("delete_task not supported".into()))
    }

    /// Pause a task by info hash.
    fn pause_task(&self, _info_hash: &str) -> Result<(), DownloaderError> {
        Err(DownloaderError::Message("pause_task not supported".into()))
    }

    /// Resume a task by info hash.
    fn resume_task(&self, _info_hash: &str) -> Result<(), DownloaderError> {
        Err(DownloaderError::Message("resume_task not supported".into()))
    }

    /// Get global speed limits. Returns (download_limit_bytes_per_sec, upload_limit_bytes_per_sec).
    /// 0 = unlimited.
    fn get_speed_limits(&self) -> Result<(u64, u64), DownloaderError> {
        Ok((0, 0))
    }

    /// Set global speed limits. 0 = unlimited.
    fn set_speed_limits(
        &self,
        _download_limit: u64,
        _upload_limit: u64,
    ) -> Result<(), DownloaderError> {
        Err(DownloaderError::Message(
            "set_speed_limits not supported".into(),
        ))
    }

    /// Live snapshot of every task the client knows about.
    ///
    /// Used by the task center to report real progress and lifecycle state for
    /// subscription-delivered tasks (which have no info hash in our store).
    /// Clients that cannot enumerate return Err; callers must not confuse
    /// unsupported snapshotting with an empty client.
    fn task_snapshots(&self) -> Result<Vec<TaskSnapshot>, DownloaderError> {
        Err(DownloaderError::Message(
            "task snapshots unsupported".into(),
        ))
    }
}

/// One client-side task as reported by [`Downloader::task_snapshots`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskSnapshot {
    /// Our marker tag in the client: the constant [`TASK_TAG`], or a legacy
    /// per-torrent `crawler-media-*` tag still present from earlier builds.
    /// Non-empty only for crawler-media deliveries — the task center uses it
    /// to filter foreign torrents out. Per-torrent identity is established by
    /// normalized title + size, not by this tag.
    pub tag: String,
    /// Client-side task name (the torrent title).
    pub name: String,
    /// 0.0..=1.0.
    pub progress: f64,
    /// Raw client state string (mapped by the caller into its own vocabulary).
    pub state: String,
    pub size_bytes: u64,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
    pub download_speed: u64,
    pub upload_speed: u64,
    pub info_hash: String,
    /// Every per-torrent ownership tag (`crawler-media-owned-<sha256>`) the
    /// client reports for this task. `tag` only carries the generic marker, so
    /// HTTP-enclosure identity must be proven through this list.
    pub owned_tags: Vec<String>,
}

/// Collect `crawler-media-owned-*` ownership tags from client tag/label values.
pub fn owned_tags_from<'a>(tags: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    tags.into_iter()
        .map(str::trim)
        .filter(|tag| tag.starts_with(OWNED_TAG_PREFIX))
        .map(str::to_string)
        .collect()
}

/// Prefix of [`ownership_tag`] values.
pub const OWNED_TAG_PREFIX: &str = "crawler-media-owned-";

/// The one tag every crawler-media delivery carries inside a download client.
///
/// A single recognizable tag (same value as the configured category) keeps the
/// client's tag list at exactly one entry no matter how many torrents we
/// deliver — no per-torrent `crawler-media-<hash>` pollution. Identity between
/// our Torrent and a client-side task is established by exact normalized title
/// + size ([`normalize_name`] / [`names_match`] / [`size_matches`]).
pub const TASK_TAG: &str = "crawler-media";

/// Review H-3: extra tag/label on a user's existing task that we claimed
/// (same info hash). Its data belongs to the user: never delete its files
/// and never Move them out of the download directory.
pub const CLAIMED_TAG: &str = "crawler-media-claimed";

/// 归一化标题（保留字母数字及中文字符、小写），用于跨客户端匹配任务。
pub fn normalize_name(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

/// 标题宽松匹配（双向包含，或分词显著重合）。
///
/// # Warning (D-L3)
/// This is a loose, display-only heuristic: unrelated tasks with similar
/// titles match. Never use it to choose a task to remove, pause, claim or
/// delete files of; destructive paths must use ownership tags / info hash.
/// `api/tests/subscription_e2e/d_l3_loose_match_audit.rs` audits call sites.
pub fn names_match(client_name: &str, want: &str) -> bool {
    let got = normalize_name(client_name);
    let want_norm = normalize_name(want);
    if !want_norm.is_empty()
        && !got.is_empty()
        && (got == want_norm || got.contains(&want_norm) || want_norm.contains(&got))
    {
        return true;
    }

    // 分词重合判断：应对 PT 站标题与 torrent 文件名中英文前后缀变体
    // （例如 PT 发帖名：Once upon a Time in Longfan S01 E01-E04 ...
    //   客户端内任务名：法医秦明之龙番往事.Once.upon.a.Time.in.Longfan.S01 ...）
    let words_got = extract_name_tokens(client_name);
    let words_want = extract_name_tokens(want);
    if !words_got.is_empty() && !words_want.is_empty() {
        let common = words_got.intersection(&words_want).count();
        let min_len = words_got.len().min(words_want.len());
        if min_len >= 2 && common * 2 >= min_len {
            return true;
        }
    }

    false
}

pub(crate) fn extract_name_tokens(s: &str) -> std::collections::HashSet<String> {
    let mut tokens = std::collections::HashSet::new();
    let mut cur = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            cur.extend(c.to_lowercase());
        } else if !cur.is_empty() {
            let tok = std::mem::take(&mut cur);
            if !is_ignored_token(&tok) {
                tokens.insert(tok);
            }
        }
    }
    if !cur.is_empty() && !is_ignored_token(&cur) {
        tokens.insert(cur);
    }
    tokens
}

fn is_ignored_token(t: &str) -> bool {
    matches!(
        t,
        "2160p"
            | "1080p"
            | "720p"
            | "480p"
            | "4k"
            | "uhd"
            | "web"
            | "dl"
            | "webdl"
            | "webrip"
            | "hdtv"
            | "bluray"
            | "h264"
            | "h265"
            | "x264"
            | "x265"
            | "hevc"
            | "avc"
            | "aac"
            | "ddp"
            | "dts"
            | "flac"
    ) || (t.starts_with('e') && t[1..].chars().all(|c| c.is_ascii_digit()))
}

/// 体积精确匹配（可选）。
pub(crate) fn size_matches(got: u64, want: Option<u64>) -> bool {
    want.is_some_and(|want| want > 0 && got == want)
}

#[cfg(test)]
mod owned_tag_tests {
    #[test]
    fn owned_tags_are_collected_and_generic_tags_ignored() {
        let owned = super::ownership_tag("https://pt.example/a");
        let raw = format!("other, crawler-media ,{owned}, crawler-media-legacy");
        assert_eq!(super::owned_tags_from(raw.split(',')), vec![owned]);
        assert!(super::owned_tags_from(["crawler-media", ""]).is_empty());
    }
}

#[cfg(test)]
mod match_tests {
    use super::torrent_matches_snapshot;

    #[test]
    fn same_size_different_title_does_not_match() {
        assert!(!torrent_matches_snapshot(
            "The Matrix 1999 1080p",
            Some(1_000_000),
            "Once Upon a Time 2024 1080p",
            1_000_000,
        ));
    }

    #[test]
    fn matching_title_and_size_matches() {
        assert!(torrent_matches_snapshot(
            "Movie.A.2024.1080p",
            Some(1_000_000),
            "Movie.A.2024.1080p",
            1_000_000,
        ));
    }

    #[test]
    fn unknown_size_still_matches_title() {
        assert!(torrent_matches_snapshot(
            "Movie.A.2024.1080p",
            None,
            "Movie.A.2024.1080p",
            1_000_000,
        ));
    }
}

/// Parse `from=to,from2=to2` path-mapping pairs from an environment value.
pub fn parse_path_maps(raw: &str) -> Vec<PathMap> {
    raw.split(',')
        .filter_map(|pair| {
            let (from, to) = pair.split_once('=')?;
            let from = from.trim();
            let to = to.trim();
            if from.is_empty() || to.is_empty() {
                return None;
            }
            Some(PathMap::new(from, to))
        })
        .collect()
}

/// Review H-2: selection must never deselect every video of a task. Returns
/// true when `names` has video files but `keep` would keep none of them.
pub(crate) fn selection_drops_all_videos<'a>(
    mut names: impl Iterator<Item = &'a str> + Clone,
    keep: &dyn Fn(&str) -> bool,
) -> bool {
    const VIDEO: [&str; 10] = [
        "mkv", "mp4", "avi", "ts", "m2ts", "wmv", "mov", "rmvb", "flv", "webm",
    ];
    let is_video = |name: &str| {
        name.rsplit_once('.')
            .is_some_and(|(_, ext)| VIDEO.contains(&ext.to_ascii_lowercase().as_str()))
    };
    names.clone().any(is_video) && !names.any(|name| is_video(name) && keep(name))
}
