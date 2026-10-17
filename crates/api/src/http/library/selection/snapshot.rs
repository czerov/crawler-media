//! 请求级浏览快照。
//!
//! 锁内**只**做数据库读取与 Clone；NFO 读取、文件 stat、过滤、排序全部在
//! 锁外执行（调用方放进 blocking 预算）。两条来源共用同一份 [`BrowseSnapshot`]：
//!
//! - 投影就绪时由 [`projected::snapshot_from_projection`] 从目标 Library 的 ready
//!   投影重建（零文件 IO）；
//! - 投影缺失/过期/不可解析时回退到 [`load_browse_snapshot`]（旧的全量读取），
//!   并打 `warn!` 留痕。

use std::collections::HashMap;
use std::time::SystemTime;

use axum::http::StatusCode;
use axum::response::Response;
use domain::{LedgerRow, Media, MediaId, UserId};

use crate::browse_file_read::BrowseFileRead;
use crate::browse_projection::ProjectionError;
use crate::http::err;
use crate::management::ApiState;
use crate::store::{CatalogCacheRow, Store, UnitState};

use super::metadata::{self, BrowseMetadata, CacheIndex};
use super::projected::{self, ProjectedFacts};
use super::{
    BROWSE_DEADLINE, LibraryFilter, LibrarySelection, LightFacts, browse_work_error, file_stats,
    projection_error_response, watch_state_from_units,
};

/// 一个 Media 及其在**当前 Library** 名下的 ledger 行。
pub(crate) struct BrowseGroup {
    pub media: Media,
    pub rows: Vec<LedgerRow>,
    /// 投影来源时为 `Some`；旧的全量来源为 `None`。
    pub projected: Option<ProjectedGroup>,
}

/// 投影里已经算好的展示事实：读路径不再做 NFO/stat。
pub(crate) struct ProjectedGroup {
    pub metadata: BrowseMetadata,
    pub missing: bool,
    pub added_at: Option<SystemTime>,
    pub size_bytes: Option<u64>,
}

/// 锁内取到的 owned 事实；此后不再需要 Store 锁。
pub(crate) struct BrowseSnapshot {
    pub library_id: String,
    pub groups: Vec<BrowseGroup>,
    pub cache: Vec<CatalogCacheRow>,
    /// path → missing_at；投影来源为空（missing 已按 Media 落到 `ProjectedGroup`）。
    pub missing: HashMap<String, i64>,
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
    let units = read_units(store, user, &ids)?;

    let mut groups = Vec::with_capacity(owned_by_media.len());
    for (id, rows) in owned_by_media {
        let Some(media) = media_by_id.get(&id).cloned() else {
            continue;
        };
        groups.push(BrowseGroup {
            media,
            rows,
            projected: None,
        });
    }

    Ok(BrowseSnapshot {
        library_id: library.id.clone(),
        groups,
        cache,
        missing,
        units,
        user,
    })
}

/// 优先读投影；投影不可用（未就绪 / 过期 / 事实不可解析）时回退全量读取。
///
/// 回退必须留痕：投影读路径与旧路径的语义由 API 级等价测试守住。
/// 容量不足 / 等待超时 / 取消**不回退**：它们不代表「投影事实不可用」，静默走全量
/// 读取只会放大慢路径；按设计返回 503/504。
pub(crate) async fn load_browse_snapshot_or_projection(
    state: &ApiState,
    library: &crate::store::Library,
    user: Option<UserId>,
) -> Result<(BrowseSnapshot, Option<ProjectedFacts>), Response> {
    match state
        .projection
        .ensure(state.clone(), library.id.clone())
        .await
    {
        Ok(snapshot) => {
            let state_clone = state.clone();
            // 投影 → 请求级快照的 DB 读（units）与组装放到 blocking 预算，
            // 不在 async worker 上持 Store 锁。
            let work = state
                .http_work
                .run(BROWSE_DEADLINE, move || -> Result<_, String> {
                    let store = state_clone.store.lock();
                    let loaded = projected::snapshot_from_projection(&store, &snapshot, user)?;
                    // 全量路径解码了整份投影：热页路径只解码本页（计数 seam）。
                    super::page::count_decoded(
                        &state_clone,
                        snapshot.files.len(),
                        snapshot.media.len(),
                    );
                    Ok(loaded)
                })
                .await;
            match work {
                Ok(Ok((snapshot, facts))) => return Ok((snapshot, Some(facts))),
                Ok(Err(error)) => tracing::warn!(
                    library_id = %library.id,
                    %error,
                    "浏览投影事实不可用，回退到全量读取"
                ),
                Err(error) => return Err(browse_work_error(error)),
            }
        }
        Err(error) => {
            if matches!(
                error,
                ProjectionError::Busy | ProjectionError::Timeout | ProjectionError::Cancelled
            ) {
                return Err(projection_error_response(error));
            }
            tracing::warn!(
                library_id = %library.id,
                ?error,
                "浏览投影未就绪，回退到全量读取"
            );
        }
    }
    // 回退路径的同步 DB 读同样移出 async worker。
    let state_clone = state.clone();
    let library = library.clone();
    let work = state
        .http_work
        .run(BROWSE_DEADLINE, move || {
            let store = state_clone.store.lock();
            load_browse_snapshot(&store, &library, user)
        })
        .await;
    match work {
        Ok(result) => result.map(|snapshot| (snapshot, None)),
        Err(error) => Err(browse_work_error(error)),
    }
}

fn read_units(
    store: &Store,
    user: Option<UserId>,
    ids: &[MediaId],
) -> Result<HashMap<(MediaId, i32, i32), UnitState>, Response> {
    let Some(user) = user else {
        return Ok(HashMap::new());
    };
    super::units_for_media(store, user, ids).map_err(|message| {
        tracing::error!(%message, "Library browse store read failed");
        err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &message)
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

/// 锁外组装：投影事实直接取用；旧路径才做 NFO/stat。
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
    for BrowseGroup {
        media,
        rows,
        projected,
    } in groups
    {
        let metadata = match &projected {
            Some(facts) => facts.metadata.clone(),
            None => metadata::load(&media, &rows, &cache, files),
        };
        let is_missing = match &projected {
            Some(facts) => facts.missing,
            None => rows.iter().any(|row| missing.contains_key(&row.path)),
        };
        let added_at = needs_added_at.then(|| match &projected {
            Some(facts) => facts.added_at,
            None => file_stats::added_at(&rows, files),
        });
        let size_bytes = needs_size.then(|| match &projected {
            Some(facts) => facts.size_bytes,
            None => file_stats::total_size(&rows, files),
        });
        let mut item = LibrarySelection {
            light: LightFacts::from_rows(&media, &metadata, &rows),
            media,
            metadata,
            missing: is_missing,
            added_at: added_at.flatten(),
            size_bytes: size_bytes.flatten(),
            last_played_at: None,
            rows,
            favorite: false,
            played: false,
            seen: false,
        };
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
