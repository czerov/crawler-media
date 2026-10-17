//! Shared ownership-safe selection for wall and gallery.
use crate::http::err;
use crate::Store;
use axum::{http::StatusCode, response::Response};
use domain::{LedgerRow, Media, MediaId, UserId};
use std::collections::HashMap;
use std::time::SystemTime;

mod file_stats;
mod filters;
pub(crate) mod metadata;
pub(crate) mod page;
pub(crate) mod projected;
mod snapshot;
pub(crate) use filters::{IndexPageOrder, LibraryFilter};
use metadata::BrowseMetadata;
pub(crate) use page::{load_browse_gallery_page, load_browse_page};
pub(crate) use projected::BrowseArtwork;
pub(crate) use snapshot::{
    load_browse_snapshot_or_projection, select_all_for_facets, select_snapshot,
};

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
    /// 过滤/排序只读这里，不读 `rows`：投影页路径在选页前没有 rows，选页后才水合。
    pub light: LightFacts,
}

/// 过滤/排序实际读取的轻事实。
///
/// 全量路径由 `rows`/`metadata` 汇总，投影页路径直接来自投影轻候选（`browse_media`
/// 的 `candidate_json`）；两条路径必须给出同一组值，由
/// `browse_projection_hot_path` 的逐字段等价比较守住。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LightFacts {
    /// 无任何外部 id（`identity` 过滤）。
    pub provisional: bool,
    /// 去重后的分辨率集合（`res` 过滤）。
    pub resolutions: Vec<String>,
    /// 任一行 hdr 非空（`hdr` 过滤）。
    pub hdr: bool,
    /// 排序键：`metadata.release_date`，缺失时按年份兜底 `YYYY-01-01`，再缺为 `""`。
    pub release_date: String,
    /// `resolution` 排序键：`resolution_rank(best_resolution(rows).unwrap_or(""))`。
    pub resolution_rank: u32,
    /// `random` 排序键：Media id 的稳定散列。
    pub shuffle_key: u64,
}

impl LightFacts {
    /// 全量路径：从 rows/metadata 汇总。
    pub(crate) fn from_rows(media: &Media, metadata: &BrowseMetadata, rows: &[LedgerRow]) -> Self {
        let mut resolutions: Vec<String> = rows
            .iter()
            .filter_map(|row| row.resolution.clone())
            .collect();
        resolutions.sort();
        resolutions.dedup();
        Self {
            provisional: is_provisional(media),
            resolutions,
            hdr: rows
                .iter()
                .any(|row| row.hdr.as_deref().is_some_and(|value| !value.is_empty())),
            release_date: release_sort_key(media, metadata),
            resolution_rank: resolution_rank(best_resolution(rows).unwrap_or("")),
            shuffle_key: shuffle_key(media.id),
        }
    }

    /// 投影轻候选：字段已按同一口径物化，这里只做同一套映射。
    pub(crate) fn from_candidate(candidate: &crate::store::BrowseMediaCandidate) -> Self {
        Self {
            provisional: candidate.provisional,
            resolutions: candidate.resolutions.clone(),
            hdr: candidate.hdr,
            release_date: candidate.release_date.clone(),
            resolution_rank: candidate
                .resolutions
                .iter()
                .map(|resolution| resolution_rank(resolution))
                .max()
                .unwrap_or_else(|| resolution_rank("")),
            shuffle_key: shuffle_key(candidate.media_id),
        }
    }
}

/// 无任何外部 id 的条目（`identity=provisional`）。
pub(crate) fn is_provisional(media: &Media) -> bool {
    media.tmdb_id.is_none()
        && media.douban_id.is_none()
        && media.tvdb_id.is_none()
        && media.bangumi_id.is_none()
        && media.anilist_id.is_none()
}

/// 与 `LibraryFilter::compare` 的 `release_date` 档同口径：完整日期优先，缺日期
/// 时按年份兜底，再缺为空串（排序仍在最前）。
pub(crate) fn release_sort_key(media: &Media, metadata: &BrowseMetadata) -> String {
    metadata.release_date.clone().unwrap_or_else(|| {
        media
            .year
            .map(|year| format!("{year:04}-01-01"))
            .unwrap_or_default()
    })
}

/// 稳定的伪随机序键：FNV-1a over the stable Media id。
pub(crate) fn shuffle_key(media_id: MediaId) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in media_id.to_string().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
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

/// 投影不可用且**不可静默回退**的失败 → HTTP 响应。
///
/// 只有容量不足 / 等待超时 / 取消映射为 503/504；facts 不可解析或 DB 错误仍由调用方
/// 回退到旧的全量读取（并 `warn!` 留痕），不能把「没建好」当成「没有授权事实」。
pub(crate) fn projection_error_response(
    error: crate::browse_projection::ProjectionError,
) -> Response {
    use crate::browse_projection::ProjectionError;
    match error {
        ProjectionError::Busy => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "投影构建容量不足，请重试",
        ),
        ProjectionError::Timeout => err(
            StatusCode::GATEWAY_TIMEOUT,
            "library.timeout",
            "浏览投影构建超时",
        ),
        ProjectionError::Cancelled => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "媒体库暂不可用，请重试",
        ),
        error => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "runtime.worker",
            &format!("{error:?}"),
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

/// 一批 Media 的 unit 状态，按 `(media, season, episode)` 索引：请求级快照与
/// 投影页水合共用同一次批读语义。
pub(crate) fn units_for_media(
    store: &Store,
    user: UserId,
    ids: &[MediaId],
) -> Result<HashMap<(MediaId, i32, i32), crate::store::UnitState>, String> {
    store
        .playback_units_for_media(user, ids)
        .map(|units| {
            units
                .into_iter()
                .map(|unit| ((unit.media_id, unit.season, unit.episode), unit.state))
                .collect()
        })
        .map_err(|error| error.to_string())
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
    watch_state_from_keys(
        media,
        rows.iter().filter_map(|row| row.season.zip(row.episode)),
        units,
    )
}

/// 与 [`watch_state_from_units`] 同一套语义，但分集键由调用方给出：投影页路径在
/// 选页前没有 rows，只有轻候选里的 `episode_keys`。
///
/// 必须逐字保留：whole `(-1,-1)` 优先、TV 每集 OR、movie legacy `(0,0)` 兜底、
/// `latest` 取最大 `updated_at`、`last_played_at` 仅在 > 0 时给出。
pub(crate) fn watch_state_from_keys(
    media: &Media,
    keys: impl IntoIterator<Item = (u32, u32)>,
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
    for (season, episode) in keys {
        let unit = units.get(&(media.id, season as i32, episode as i32));
        state.seen |= unit.is_some_and(|u| u.played || u.position_ms > 0 || u.play_count > 0);
        if let Some(unit) = unit {
            state.favorite |= unit.favorite;
            latest = latest.max(unit.updated_at);
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
