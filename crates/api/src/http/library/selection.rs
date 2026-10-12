//! Shared ownership-safe selection for wall and gallery.
use crate::http::err;
use crate::{Store, store::Library};
use axum::{http::StatusCode, response::Response};
use domain::{LedgerRow, Media, MediaId, UserId};
use std::collections::HashMap;
use std::time::SystemTime;

mod file_stats;
mod filters;
mod metadata;
mod snapshot;
pub(crate) use filters::LibraryFilter;
use metadata::BrowseMetadata;
pub(crate) use snapshot::{load_browse_snapshot, select_all_for_facets, select_snapshot};

pub(crate) struct LibrarySelection {
    pub media: Media,
    pub rows: Vec<LedgerRow>,
    pub metadata: BrowseMetadata,
    pub favorite: bool,
    pub played: bool,
    pub seen: bool,
    pub missing: bool,
    pub added_at: Option<SystemTime>,
    pub size_bytes: Option<u64>,
    pub last_played_at: Option<i64>,
}

/// 浏览路径的等待预算：NFO/stat 可能落在网络存储上，给足余量但不无限等待。
pub(crate) const BROWSE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// blocking 预算失败 → HTTP 响应（只读浏览路径）。
pub(crate) fn browse_work_error(error: crate::blocking_budget::BlockingError) -> Response {
    use crate::blocking_budget::BlockingError;
    match error {
        BlockingError::Busy => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请重试",
        ),
        BlockingError::Timeout => err(
            StatusCode::GATEWAY_TIMEOUT,
            "library.timeout",
            "浏览请求超时",
        ),
        error => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "runtime.worker",
            &error.to_string(),
        ),
    }
}

/// 一部作品的观看状态。首页库行/收藏行的「未看优先」分级、筛选条的「未观看 / 在看 /
/// 已看完」三档，都读这一份，避免同一部作品在两个地方被分成不同的档。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WatchState {
    pub favorite: bool,
    pub played: bool,
    pub seen: bool,
    pub last_played_at: Option<i64>,
}

/// `played` ＝作品级的"已看完"标记；`seen` ＝动过（整剧级或任一集有进度/次数/看完）。
pub(crate) fn watch_state_for<'a>(
    store: &Store,
    user: UserId,
    media: &Media,
    rows: impl IntoIterator<Item = &'a LedgerRow>,
) -> Result<WatchState, String> {
    let mut state = WatchState::default();
    let mut latest = 0i64;
    let whole = store
        .unit_state(
            user,
            media.id,
            crate::store::UNIT_WHOLE,
            crate::store::UNIT_WHOLE,
        )
        .map_err(|e| e.to_string())?;
    state.favorite = whole.as_ref().is_some_and(|u| u.favorite);
    state.played = whole.as_ref().is_some_and(|u| u.played);
    state.seen = whole
        .as_ref()
        .is_some_and(|u| u.played || u.position_ms > 0 || u.play_count > 0);
    if let Some(unit) = &whole {
        latest = latest.max(unit.updated_at);
    }
    for row in rows {
        if let (Some(season), Some(episode)) = (row.season, row.episode) {
            let unit = store
                .unit_state(user, media.id, season as i32, episode as i32)
                .map_err(|e| e.to_string())?;
            state.seen |= unit
                .as_ref()
                .is_some_and(|u| u.played || u.position_ms > 0 || u.play_count > 0);
            if let Some(unit) = &unit {
                state.favorite |= unit.favorite;
                latest = latest.max(unit.updated_at);
            }
        }
    }
    if media.kind == domain::MediaKind::Movie && !state.favorite {
        if let Ok(Some(legacy_unit)) = store.unit_state(user, media.id, 0, 0) {
            state.favorite |= legacy_unit.favorite;
            state.played |= legacy_unit.played;
            state.seen |=
                legacy_unit.played || legacy_unit.position_ms > 0 || legacy_unit.play_count > 0;
            latest = latest.max(legacy_unit.updated_at);
        }
    }
    state.last_played_at = (latest > 0).then_some(latest);
    Ok(state)
}

/// 与 [`watch_state_for`] 同一套语义，但读的是请求级快照里的 unit map，
/// 不再每个 episode 打一次数据库。
///
/// 必须逐字保留：whole `(-1,-1)` 优先、TV 每集 OR、movie legacy `(0,0)` 兜底、
/// `latest` 取最大 `updated_at`、`last_played_at` 仅在 > 0 时给出。
pub(crate) fn watch_state_from_units(
    media: &Media,
    rows: &[LedgerRow],
    units: &HashMap<(MediaId, i32, i32), crate::store::UnitState>,
) -> WatchState {
    let mut state = WatchState::default();
    let mut latest = 0i64;
    let whole = units.get(&(media.id, crate::store::UNIT_WHOLE, crate::store::UNIT_WHOLE));
    state.favorite = whole.is_some_and(|u| u.favorite);
    state.played = whole.is_some_and(|u| u.played);
    state.seen = whole.is_some_and(|u| u.played || u.position_ms > 0 || u.play_count > 0);
    if let Some(unit) = whole {
        latest = latest.max(unit.updated_at);
    }
    for row in rows {
        if let (Some(season), Some(episode)) = (row.season, row.episode) {
            let unit = units.get(&(media.id, season as i32, episode as i32));
            state.seen |= unit.is_some_and(|u| u.played || u.position_ms > 0 || u.play_count > 0);
            if let Some(unit) = unit {
                state.favorite |= unit.favorite;
                latest = latest.max(unit.updated_at);
            }
        }
    }
    if media.kind == domain::MediaKind::Movie && !state.favorite {
        if let Some(legacy_unit) = units.get(&(media.id, 0, 0)) {
            state.favorite |= legacy_unit.favorite;
            state.played |= legacy_unit.played;
            state.seen |=
                legacy_unit.played || legacy_unit.position_ms > 0 || legacy_unit.play_count > 0;
            latest = latest.max(legacy_unit.updated_at);
        }
    }
    state.last_played_at = (latest > 0).then_some(latest);
    state
}

pub(crate) fn resolution_rank(value: &str) -> u32 {
    match value {
        "2160p" | "4k" => 4,
        "1080p" | "1080i" => 3,
        "720p" => 2,
        _ => 1,
    }
}
pub(crate) fn best_resolution(rows: &[LedgerRow]) -> Option<&str> {
    rows.iter()
        .filter_map(|r| r.resolution.as_deref())
        .max_by_key(|r| resolution_rank(r))
}

fn store_error(error: crate::store::StoreError) -> Response {
    tracing::error!(%error, "Library browse store read failed");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}
