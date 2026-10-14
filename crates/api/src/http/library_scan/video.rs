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

/// 为刚入账的 Video 文件附着 poster/backdrop。
///
/// 用真实 Media（而不是临时构造的假 Media），这样用户对 `artwork.{media_id}` 的
/// 海报锁仍然生效。每处理一个文件前检查一次取消，避免一个 chunk 内的截帧工作
/// 无视 Library 冻结/删除。真实失败由 `poster_fetch` 内部按 warn 记录；没有可写
/// 的图不是错误。
pub(crate) fn attach_video_artwork(
    state: &ApiState,
    inserted: &[PathBuf],
    control: &library::WorkControl,
) {
    for path in inserted {
        if control.is_cancelled() {
            tracing::info!(
                remaining = inserted.len(),
                "扫描已取消，停止为剩余 Video 文件附着封面"
            );
            return;
        }
        let Some(media) = media_for_path(state, path) else {
            continue;
        };
        let _ = crate::poster_fetch::attach_poster(state, &media, path);
        let _ = crate::poster_fetch::attach_backdrop(state, &media, path);
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
