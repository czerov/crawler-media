use serde_json::{Value, json};
use std::path::{Path as FsPath, PathBuf};

use super::library_for_row;
use crate::store::{Library, LibraryRemoval};

/// Ledger rows whose path falls under any root of the library.
pub(crate) fn rows_in_library(store: &crate::Store, library: &Library) -> Vec<domain::LedgerRow> {
    store
        .list_ledger()
        .unwrap_or_default()
        .into_iter()
        .filter(|row| {
            let Some(media) = store.get_media(row.media_id).ok().flatten() else {
                return false;
            };
            library_for_row(store, row, &media).is_some_and(|owner| owner.id == library.id)
        })
        .collect()
}

/// 封面决策的**路径输入**：只有 DB/内存事实，没有任何 `is_file` 观察。
struct CoverInput {
    /// 用户显式禁用封面（`cover_path` 为 `"none"`/空串）：不回填 legacy。
    disabled: bool,
    /// 显式配置的封面路径。
    explicit: Option<PathBuf>,
    /// legacy 固定路径 `library-{id}.jpg`。
    legacy: PathBuf,
}

/// 一个 Library 在**锁内**收集到的 owned DB 事实。
///
/// 这里刻意不含任何文件系统观察：`stat`/`is_dir`/`is_file` 在锁外（受 blocking
/// 预算约束）才做，慢挂载不会阻塞共享 Store。统计（计数/体积/刷新状态）也不再
/// 在这里逐库重算：由 [`crate::library_statistics`] 一次聚合提供。
pub(crate) struct LibraryFacts {
    pub(crate) library: Library,
    removal: Option<LibraryRemoval>,
    cover: CoverInput,
}

/// 锁内读取：removal 状态、封面候选路径。全部是 DB 或内存事实。
pub(crate) fn library_facts(store: &crate::Store, library: &Library) -> LibraryFacts {
    let cover = CoverInput {
        disabled: matches!(library.cover_path.as_deref(), Some("none") | Some("")),
        explicit: library.cover_path.as_deref().and_then(|path| {
            // `"none"`/空串是「显式禁用」，不是路径。
            (!path.is_empty() && path != "none").then(|| PathBuf::from(path))
        }),
        legacy: store
            .library_covers_dir()
            .join(format!("library-{}.jpg", library.id)),
    };
    LibraryFacts {
        library: library.clone(),
        removal: store.removal_for_library(&library.id).ok().flatten(),
        cover,
    }
}

/// 与 `library_cover_path` 同一套只读语义，但路径输入已经在锁内算好。
fn cover_path_from_input(cover: &CoverInput) -> Option<PathBuf> {
    if cover.disabled {
        return None;
    }
    if let Some(path) = &cover.explicit {
        if path.is_file() {
            return Some(path.clone());
        }
    }
    cover.legacy.is_file().then(|| cover.legacy.clone())
}

/// 锁外：在 owned 事实之上做文件观察并渲染响应 JSON。
///
/// `stats` 由 [`crate::library_statistics`] 一次聚合给出（计数来自台账、体积来自
/// 投影 facts、刷新状态来自 head）；这里只负责逐 root `is_dir` 与封面 `is_file`。
pub(crate) fn library_json_from_facts(
    facts: &LibraryFacts,
    stats: Option<crate::library_statistics::LibraryStatistics>,
) -> Value {
    let stats = stats.unwrap_or(crate::library_statistics::LibraryStatistics {
        stale: true,
        ..Default::default()
    });
    let library = &facts.library;
    // 根目录被删：库不能假装是「空的」，要在 UI 提示并提供恢复/删除。
    let root_missing = library
        .root_paths
        .iter()
        .any(|root| !FsPath::new(root).is_dir());
    json!({
        "id": library.id,
        "removal": facts.removal,
        "kind": library.kind.as_str(),
        "name": library.name,
        "root_paths": library.root_paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "root_missing": root_missing,
        "is_default": library.is_default,
        "access_mode": library.access_mode,
        "admin_visible": library.admin_visible,
        "member_ids": library.member_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
        "match_rules": library.match_rules,
        "default_filter_id": library.default_filter_id,
        "detect_intros": library.detect_intros,
        "enable_fingerprint": library.enable_fingerprint,
        "realtime_watch": library.realtime_watch,
        "generate_thumbnails": library.generate_thumbnails,
        "extract_chapter_images": library.extract_chapter_images,
        "exclude_from_home": library.exclude_from_home,
        "auto_series_collections": library.auto_series_collections,
        "cover_url": cover_path_from_input(&facts.cover).map(|_| format!("/libraries/{}/cover", library.id)),
        "stats": {
            "item_count": stats.item_count,
            "file_count": stats.file_count,
            "total_size_bytes": stats.total_size_bytes,
            "strm_file_count": stats.strm_file_count,
            "refreshed_at_ms": stats.refreshed_at_ms,
            "stale": stats.stale,
        },
    })
}

/// 同步版本：库配置的写路径仍在锁内直接渲染（不在本次改动范围内），统计用
/// [`crate::library_statistics::statistics_for`] 的同一套口径同步算一次，不构建。
pub(crate) fn library_json(store: &crate::Store, library: &Library) -> Value {
    let stats = crate::library_statistics::statistics_for(store, library);
    library_json_from_facts(&library_facts(store, library), Some(stats))
}

/// Emby-style current cover source: latest TV episode wins; movies use their
/// sole file. This makes a newly imported episode refresh the series cover.
pub(crate) fn preferred_row<'a, R>(rows: &'a [R]) -> Option<&'a domain::LedgerRow>
where
    R: std::borrow::Borrow<domain::LedgerRow>,
{
    rows.iter()
        .map(std::borrow::Borrow::borrow)
        .max_by_key(|row| {
            (
                row.season.unwrap_or(0),
                row.episode.unwrap_or(0),
                row.path.clone(),
            )
        })
}

pub(crate) fn poster_path(row: &domain::LedgerRow) -> Option<PathBuf> {
    poster_path_with(row, &crate::browse_file_read::LocalBrowseFileRead)
}

/// 与 [`poster_path`] 同一套布局，存在性判断走注入的浏览文件 seam。
pub(crate) fn poster_path_with(
    row: &domain::LedgerRow,
    files: &dyn crate::browse_file_read::BrowseFileRead,
) -> Option<PathBuf> {
    if let Some(poster) = crate::poster::poster_beside(&row.path, files) {
        return Some(poster);
    }
    let file_dir = FsPath::new(&row.path).parent()?;
    // 兼容两种布局：平铺（文件旁 poster.jpg）与模板整理后
    // （<show>/Season N/file → show 根 poster.jpg，向上找一层）。
    let up = file_dir.parent()?.join("poster.jpg");
    files.is_file(&up).then_some(up)
}
