//! Shared ownership-safe selection for wall and gallery.
use crate::http::err;
use crate::{Store, store::Library};
use axum::{http::StatusCode, response::Response};
use domain::{LedgerRow, Media, MediaId, UserId};
use std::collections::HashMap;
use std::time::SystemTime;

mod filters;
mod metadata;
mod file_stats;
pub(crate) use filters::LibraryFilter;
use metadata::BrowseMetadata;
use file_stats::{added_at, total_size};

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

pub(crate) fn select(
    store: &Store,
    library: &Library,
    user: Option<UserId>,
    query: &HashMap<String, String>,
) -> Result<Vec<LibrarySelection>, Response> {
    let filter = LibraryFilter::parse(query).map_err(|message| {
        tracing::error!(%message, "Library browse query rejected");
        err(StatusCode::BAD_REQUEST, "library.invalid", &message)
    })?;
    let mut grouped: HashMap<MediaId, Vec<LedgerRow>> = HashMap::new();
    for row in store.list_ledger().map_err(store_error)? {
        grouped.entry(row.media_id).or_default().push(row);
    }
    let missing = store.missing_at_by_path().map_err(store_error)?;
    let cache = store.list_catalog_cache().map_err(store_error)?;
    let mut items = Vec::new();
    for (id, rows) in grouped {
        let Some(media) = store.get_media(id).map_err(store_error)? else {
            continue;
        };
        let mut owned = Vec::new();
        for row in rows {
            if store
                .library_for_path(std::path::Path::new(&row.path), media.kind)
                .map_err(store_error)?
                .is_some_and(|owner| owner.id == library.id)
            {
                owned.push(row);
            }
        }
        if owned.is_empty() {
            continue;
        }
        let metadata = metadata::load(&media, &owned, &cache);
        let mut item = LibrarySelection {
            media,
            metadata,
            missing: owned.iter().any(|r| missing.contains_key(&r.path)),
            added_at: None,
            size_bytes: None,
            last_played_at: None,
            rows: owned,
            favorite: false,
            played: false,
            seen: false,
        };
        if filter.sort == "added_at" {
            item.added_at = added_at(&item.rows);
        }
        if filter.sort == "size" {
            item.size_bytes = total_size(&item.rows);
        }
        if let Some(user) = user {
            match store.unit_state(
                user,
                item.media.id,
                crate::store::UNIT_WHOLE,
                crate::store::UNIT_WHOLE,
            ) {
                Ok(Some(whole)) => item.favorite = whole.favorite,
                Ok(None) => {}
                Err(error) => {
                    tracing::error!(%error, media_id = %item.media.id, "Library favorite state read failed");
                    return Err(store_error(error));
                }
            }
        }
        // Detailed watch state (played/seen/last_played per episode) costs extra queries;
        // only read it when the request actually depends on it.
        if filter.needs_watch_state() {
            if let Some(user) = user {
                let state = watch_state_for(store, user, &item.media, &item.rows).map_err(
                    |message| {
                        tracing::error!(%message, "Library watch state read failed");
                        err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &message)
                    },
                )?;
                item.favorite = state.favorite;
                item.played = state.played;
                item.seen = state.seen;
                item.last_played_at = state.last_played_at;
            }
        }
        if filter.matches(&item) {
            items.push(item);
        }
    }
    items.sort_by(|a, b| filter.compare(a, b));
    Ok(items)
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
        .unit_state(user, media.id, crate::store::UNIT_WHOLE, crate::store::UNIT_WHOLE)
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
