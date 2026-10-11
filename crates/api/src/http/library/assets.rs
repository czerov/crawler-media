use serde_json::{Value, json};
use std::path::{Path as FsPath, PathBuf};

use super::{library_cover_path, library_for_row};
use crate::store::Library;

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

fn file_size(path: &str) -> u64 {
    // 对于 .strm 虚拟指针文件，磁盘只有几十字节的 URL 文本，
    // 不计入本地物理体积统计（避免 10 部 4K 剧集显示 1 KB 的怪异现象）
    if path.to_lowercase().ends_with(".strm") {
        return 0;
    }
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

pub(crate) fn library_json(store: &crate::Store, library: &Library) -> Value {
    let rows = rows_in_library(store, library);
    let bytes: u64 = rows.iter().map(|row| file_size(&row.path)).sum();
    let strm_count = rows
        .iter()
        .filter(|row| row.path.to_lowercase().ends_with(".strm"))
        .count();
    let mut media_ids = std::collections::HashSet::new();
    for row in &rows {
        media_ids.insert(row.media_id);
    }
    // 根目录被删：库不能假装是「空的」，要在 UI 提示并提供恢复/删除。
    let root_missing = library
        .root_paths
        .iter()
        .any(|root| !FsPath::new(root).is_dir());
    json!({
        "id": library.id,
        "removal": store.removal_for_library(&library.id).ok().flatten(),
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
        "cover_url": library_cover_path(store, library).map(|_| format!("/libraries/{}/cover", library.id)),
        "stats": {
            "item_count": media_ids.len() as i64,
            "file_count": rows.len() as i64,
            "total_size_bytes": bytes,
            "strm_file_count": strm_count as i64,
        },
    })
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
    if let Some(poster) = crate::poster::poster_beside(&row.path) {
        return Some(poster);
    }
    let file_dir = FsPath::new(&row.path).parent()?;
    // 兼容两种布局：平铺（文件旁 poster.jpg）与模板整理后
    // （<show>/Season N/file → show 根 poster.jpg，向上找一层）。
    let poster = file_dir.parent()?.join("poster.jpg");
    poster.is_file().then_some(poster)
}
