//! Video（「其他」库）扫描的专属语义。
//!
//! Video 与 Movie/TV 共用 [`super::runner`] 的有界执行器（迭代遍历、100 文件
//! chunk、`WorkControl` 取消、`scan_seen` 断点）。这里只保留 Video 与 Movie/TV
//! 不同的两点：
//!
//! 1. 登记：每个视频文件一个 Media（标题 = 文件名主干，kind=video），不做识别/
//!    刮削；由 `runner::walk` 调用 `watch_ledger::record_video_paths`。
//! 2. 封面：登记后为文件附着 poster/backdrop。
//!
//! 旧的独立递归实现（整库 collect + clone 后再登记）已删除：它绕过 chunk、取消与
//! 断点，还会把定向扫描静默降级成整库扫描（P3-2）。

use std::path::{Path, PathBuf};

use crate::management::ApiState;

/// 为本轮处理过的 Video 文件附着 poster/backdrop。
///
/// `candidates` 是**封面候选集**，不是探测候选集：包含本 chunk 过滤后真正处理过的
/// 全部文件（含已入账、本轮未变化的），这样镜像图片后开启、上次失败重试、用户删掉
/// 自动海报等场景都能在普通重扫里补上（P2-1）。断点跳过的文件不在候选集里。
///
/// 用真实 Media（而不是临时构造的假 Media），这样用户对 `artwork.{media_id}` 的
/// 海报锁仍然生效。每处理一个文件前检查一次取消，避免一个 chunk 内的截帧工作
/// 无视 Library 冻结/删除。真实失败由 `poster_fetch` 内部按 warn 记录；没有可写
/// 的图不是错误。
pub(crate) fn attach_video_artwork(
    state: &ApiState,
    candidates: &[PathBuf],
    control: &library::WorkControl,
) {
    let mut poster_completed_dirs = std::collections::HashSet::new();
    let mut backdrop_completed_targets = std::collections::HashSet::new();
    for path in candidates {
        if control.is_cancelled() {
            tracing::info!(
                remaining = candidates.len(),
                "扫描已取消，停止为剩余 Video 文件附着封面"
            );
            return;
        }
        let Some(media) = media_for_path(state, path) else {
            continue;
        };
        // 海报去重：按视频父目录（poster.jpg 目标目录）分别跟踪完成状态；
        // 首个候选失败且目标仍缺失时，允许同目录后续视频重试。
        if let Some(dir) = path.parent() {
            if !poster_completed_dirs.contains(dir) {
                let outcome = crate::poster_fetch::attach_poster_outcome_if_missing(state, &media, path);
                match outcome {
                    crate::poster_fetch::ArtworkOutcome::Written
                    | crate::poster_fetch::ArtworkOutcome::AlreadyPresent => {
                        poster_completed_dirs.insert(dir.to_path_buf());
                    }
                    crate::poster_fetch::ArtworkOutcome::Skipped
                    | crate::poster_fetch::ArtworkOutcome::Failed => {}
                }
            } else {
                tracing::debug!(
                    dir = %dir.display(),
                    path = %path.display(),
                    "同目录已有视频成功完成海报，跳过当前文件"
                );
            }
        }

        // 背景图去重：按实际目标路径（fanart.jpg）分别跟踪完成状态
        // （Season* 目录的背景图写到上级目录，不等于视频父目录）；
        // 首个候选失败且目标仍缺失时，允许后续候选重试。
        if let Some(target) = crate::poster_fetch::backdrop_target(path) {
            if !backdrop_completed_targets.contains(&target) {
                let outcome = crate::poster_fetch::attach_backdrop_outcome(state, &media, path);
                match outcome {
                    crate::poster_fetch::ArtworkOutcome::Written
                    | crate::poster_fetch::ArtworkOutcome::AlreadyPresent => {
                        backdrop_completed_targets.insert(target);
                    }
                    crate::poster_fetch::ArtworkOutcome::Skipped
                    | crate::poster_fetch::ArtworkOutcome::Failed => {}
                }
            } else {
                tracing::debug!(
                    target = %target.display(),
                    path = %path.display(),
                    "目标背景图已由其它视频完成，跳过当前文件"
                );
            }
        }
    }
}

/// 路径 → 已入账的 Media（登记阶段刚写过台账行，正常都应命中）。
fn media_for_path(state: &ApiState, path: &Path) -> Option<domain::Media> {
    let store = state.store.lock();
    let row = store
        .ledger_by_path(&path.display().to_string())
        .ok()
        .flatten()?;
    store.get_media(row.media_id).ok().flatten()
}
