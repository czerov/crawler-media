//! 请求级浏览快照。
//!
//! 锁内**只**做数据库读取与 Clone；NFO 读取、文件 stat、过滤、排序全部在
//! 锁外执行（调用方放进 blocking 预算）。这样一次浏览请求不再逐行重读
//! Library 配置、不再每个 Media 打一次 `get_media`、也不在锁内碰磁盘。

use std::collections::HashMap;

use axum::http::StatusCode;
use axum::response::Response;
use domain::{LedgerRow, Media, MediaId, UserId};

use crate::browse_file_read::BrowseFileRead;
use crate::http::err;
use crate::store::{CatalogCacheRow, LibraryOwnership, Store, UnitState};

use super::metadata::{self, CacheIndex};
use super::{LibraryFilter, LibrarySelection, file_stats, watch_state_from_units};

/// 一个 Media 及其在**当前 Library** 名下的 ledger 行。
pub(crate) struct BrowseGroup {
    pub media: Media,
    pub rows: Vec<LedgerRow>,
}

/// 锁内取到的 owned 事实；此后不再需要 Store 锁。
pub(crate) struct BrowseSnapshot {
    pub library_id: String,
    pub ownership: LibraryOwnership,
    pub groups: Vec<BrowseGroup>,
    pub missing: HashMap<String, i64>,
    pub cache: Vec<CatalogCacheRow>,
    pub units: HashMap<(MediaId, i32, i32), UnitState>,
    pub user: Option<UserId>,
}

/// 锁内执行：一次 ownership 快照 + 一次 ledger + 一次批量 Media + 一次
/// missing/cache + 一次该用户的 unit 读取。任何 DB 错误都向上抛。
pub(crate) fn load_browse_snapshot(
    store: &Store,
    library: &crate::store::Library,
    user: Option<UserId>,
) -> Result<BrowseSnapshot, Response> {
    let ownership = store.library_ownership().map_err(super::store_error)?;
    let missing = store.missing_at_by_path().map_err(super::store_error)?;
    let cache = store.list_catalog_cache().map_err(super::store_error)?;
    let units = match user {
        Some(user) => store
            .playback_units_for_user(user)
            .map_err(super::store_error)?
            .into_iter()
            .map(|unit| ((unit.media_id, unit.season, unit.episode), unit.state))
            .collect(),
        None => HashMap::new(),
    };

    // 归属用快照纯函数判定：不再每行调一次 `library_for_path`（它会重新读全库配置）。
    let mut owned_by_media: HashMap<MediaId, Vec<LedgerRow>> = HashMap::new();
    for row in store.list_ledger().map_err(super::store_error)? {
        let owner = ownership.owner(std::path::Path::new(&row.path), library.kind);
        if owner.is_some_and(|owner| owner.id == library.id) {
            owned_by_media.entry(row.media_id).or_default().push(row);
        }
    }
    let ids: Vec<MediaId> = owned_by_media.keys().copied().collect();
    let media_by_id = store.media_batch(&ids).map_err(super::store_error)?;

    let mut groups = Vec::with_capacity(owned_by_media.len());
    for (id, rows) in owned_by_media {
        let Some(media) = media_by_id.get(&id).cloned() else {
            continue;
        };
        groups.push(BrowseGroup { media, rows });
    }

    Ok(BrowseSnapshot {
        library_id: library.id.clone(),
        ownership,
        groups,
        missing,
        cache,
        units,
        user,
    })
}

/// 锁外执行：NFO/stat、过滤、排序。分页由调用方在排序之后做。
pub(crate) fn select_snapshot(
    snapshot: BrowseSnapshot,
    query: &HashMap<String, String>,
    files: &dyn BrowseFileRead,
) -> Result<Vec<LibrarySelection>, Response> {
    let filter = LibraryFilter::parse(query).map_err(|message| {
        tracing::error!(%message, "Library browse query rejected");
        err(StatusCode::BAD_REQUEST, "library.invalid", &message)
    })?;
    let mut items = collect_items(
        snapshot,
        files,
        filter.sort == "added_at",
        filter.sort == "size",
        filter.needs_watch_state(),
    );
    items.retain(|item| filter.matches(item));
    items.sort_by(|a, b| filter.compare(a, b));
    Ok(items)
}

/// facets 用：不做筛选（候选全集是整库可见 Media），但**强制**计算 watch state——
/// 默认 filter 不会触发它，而候选与计数都要 `played`/`seen`/`favorite`。
pub(crate) fn select_all_for_facets(
    snapshot: BrowseSnapshot,
    files: &dyn BrowseFileRead,
) -> Vec<LibrarySelection> {
    collect_items(snapshot, files, false, false, true)
}

/// 锁外组装：NFO/stat 与观看状态都只在这里发生。
fn collect_items(
    snapshot: BrowseSnapshot,
    files: &dyn BrowseFileRead,
    needs_added_at: bool,
    needs_size: bool,
    needs_watch_state: bool,
) -> Vec<LibrarySelection> {
    let cache = CacheIndex::build(&snapshot.cache);
    let BrowseSnapshot {
        groups,
        missing,
        units,
        user,
        ..
    } = snapshot;

    let mut items = Vec::new();
    for BrowseGroup { media, rows } in groups {
        let metadata = metadata::load(&media, &rows, &cache, files);
        let mut item = LibrarySelection {
            media,
            metadata,
            missing: rows.iter().any(|row| missing.contains_key(&row.path)),
            added_at: None,
            size_bytes: None,
            last_played_at: None,
            rows,
            favorite: false,
            played: false,
            seen: false,
        };
        if needs_added_at {
            item.added_at = file_stats::added_at(&item.rows, files);
        }
        if needs_size {
            item.size_bytes = file_stats::total_size(&item.rows, files);
        }
        if let Some(_user) = user {
            // 收藏状态永远来自 whole unit；详细观看状态只在请求需要时才算。
            let whole = units.get(&(
                item.media.id,
                crate::store::UNIT_WHOLE,
                crate::store::UNIT_WHOLE,
            ));
            item.favorite = whole.is_some_and(|unit| unit.favorite);
            if needs_watch_state {
                let state = watch_state_from_units(&item.media, &item.rows, &units);
                item.favorite = state.favorite;
                item.played = state.played;
                item.seen = state.seen;
                item.last_played_at = state.last_played_at;
            }
        }
        items.push(item);
    }
    items
}
