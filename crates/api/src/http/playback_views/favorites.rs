use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

use crate::http::library::{self, poster_path};
use crate::http::ok;
use crate::management::ApiState;
use ::library::WatchTier;

/// 收藏行的一条：作品 + 收藏所在的层级（最近一次收藏动作的单元）。
struct FavoriteEntry {
    media_id: domain::MediaId,
    /// 最近一次收藏动作的时间（默认档按它排）
    updated_at: i64,
    season: i32,
    episode: i32,
    /// 未看优先才填：未看 → 在看 → 已看完
    tier: Option<WatchTier>,
}

/// GET /playback/favorites?limit&offset&unwatched_first&sort&order
/// — distinct favorited media from `playback_units`, level = most recent unit.
pub(crate) async fn favorites(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let limit = query
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20)
        .clamp(1, 200);
    let offset = query
        .get("offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let sort_key = query
        .get("sort")
        .map(String::as_str)
        .unwrap_or("updated_at");
    // 当前端指定按 title 排序且未传 order 时，默认按 A->Z 升序排列；其他排序默认按倒序
    let is_desc = query
        .get("order")
        .map(|o| o == "desc")
        .unwrap_or_else(|| sort_key != "title");
    // 未看优先：观看分级参与排序，但不筛掉任何收藏（见 library::latest 的说明）
    let unwatched_first = query
        .get("unwatched_first")
        .is_some_and(|v| v == "true" || v == "1");
    struct FavoriteCandidate {
        media: domain::Media,
        library_id: String,
        preferred_row: Option<domain::LedgerRow>,
        file_count: usize,
        seasons: std::collections::BTreeSet<u32>,
        episode_count: i64,
        favorite_season_number: Option<i32>,
        favorite_episode_number: Option<i32>,
    }

    let (candidates, total) = {
        let store = state.store.lock();
        let all_rows = store.list_ledger().unwrap_or_default();
        let mut list = favorite_entries(&store, user_id, &all_rows, unwatched_first);
        sort_favorite_entries(&store, &mut list, sort_key, is_desc, unwatched_first);
        let total = list.len();
        let cands: Vec<FavoriteCandidate> = list
            .into_iter()
            .skip(offset)
            .take(limit)
            .filter_map(|entry| {
                let media_id = entry.media_id;
                let media = store.get_media(media_id).ok().flatten()?;
                let owned: Vec<domain::LedgerRow> = all_rows
                    .iter()
                    .filter(|r| r.media_id == media_id)
                    .filter(|r| crate::http::library::row_visible_to_user(&store, r, &media, Some(user_id)))
                    .cloned()
                    .collect();
                if owned.is_empty() {
                    return None;
                }
                let refs: Vec<&domain::LedgerRow> = owned.iter().collect();
                let library_id = refs
                    .first()
                    .and_then(|row| crate::http::library::library_for_row(&store, row, &media))
                    .map(|library| library.id)
                    .unwrap_or_default();
                let pref = crate::http::library::preferred_row(&refs).cloned();
                let seasons = owned.iter().filter_map(|r| r.season).collect::<std::collections::BTreeSet<_>>();
                let episode_count = owned.iter().filter(|r| r.episode.is_some()).count() as i64;
                Some(FavoriteCandidate {
                    media,
                    library_id,
                    preferred_row: pref,
                    file_count: owned.len(),
                    seasons,
                    episode_count,
                    favorite_season_number: (entry.season >= 0).then_some(entry.season),
                    favorite_episode_number: (entry.episode >= 0).then_some(entry.episode),
                })
            })
            .collect();
        (cands, total)
    };

    let files = state.browse_files.clone();
    let page_res = state
        .http_work
        .run(std::time::Duration::from_secs(30), move || {
            let mut items = Vec::new();
            for cand in candidates {
                let poster = cand
                    .preferred_row
                    .as_ref()
                    .and_then(|row| crate::http::library::poster_path_with(row, files.as_ref()))
                    .map(|_| {
                        let row = cand.preferred_row.as_ref().expect("checked");
                        crate::http::library::artwork_url("posters", row.id)
                    });
                items.push(json!({
                    "media_item_id": cand.media.id.to_string(),
                    "kind": cand.media.kind.as_str(),
                    "library_id": cand.library_id,
                    "title": cand.media.title,
                    "year": cand.media.year,
                    "poster_url": poster,
                    "file_count": cand.file_count,
                    "seasons": cand.seasons,
                    "episode_count": cand.episode_count,
                    "favorite_season_number": cand.favorite_season_number,
                    "favorite_episode_number": cand.favorite_episode_number,
                }));
            }
            items
        })
        .await;

    let page = match page_res {
        Ok(items) => items,
        Err(crate::blocking_budget::BlockingError::Busy) => {
            return crate::http::err(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "runtime.busy",
                "服务繁忙，请重试",
            );
        }
        Err(crate::blocking_budget::BlockingError::Timeout) => {
            return crate::http::err(
                axum::http::StatusCode::GATEWAY_TIMEOUT,
                "favorites.timeout",
                "收藏列表加载超时",
            );
        }
        Err(crate::blocking_budget::BlockingError::Worker(msg)) => {
            return crate::http::err(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "favorites.worker",
                &msg,
            );
        }
    };
    ok(json!({ "items": page, "total": total })).into_response()
}

/// 当前身份可见的收藏作品；`with_tier` 决定要不要顺带算未看优先用的观看分级
/// （那一趟是逐单元的存储读取，只有开了未看优先才付这份钱）。
fn favorite_entries(
    store: &crate::Store,
    user_id: domain::UserId,
    all_rows: &[domain::LedgerRow],
    with_tier: bool,
) -> Vec<FavoriteEntry> {
    let media_ids: HashSet<domain::MediaId> = all_rows
        .iter()
        .filter_map(|row| {
            let media = store.get_media(row.media_id).ok().flatten()?;
            crate::http::library::row_visible_to_user(store, row, &media, Some(user_id))
                .then_some(row.media_id)
        })
        .collect();
    let mut by_media: HashMap<domain::MediaId, (i64, i32, i32)> = HashMap::new(); // updated_at, season, episode
    for media_id in media_ids {
        for row in store.unit_rows(user_id, media_id).unwrap_or_default() {
            if !row.favorite {
                continue;
            }
            let entry = by_media
                .entry(media_id)
                .or_insert((0, row.season, row.episode));
            if row.updated_at > entry.0 {
                *entry = (row.updated_at, row.season, row.episode);
            }
        }
    }
    by_media
        .into_iter()
        .map(|(media_id, (updated_at, season, episode))| FavoriteEntry {
            media_id,
            updated_at,
            season,
            episode,
            tier: with_tier.then(|| favorite_tier(store, user_id, all_rows, media_id)),
        })
        .collect()
}

/// 一部作品的观看分级。与库行、筛选条共用 `selection::watch_state_for`：同一部作品
/// 不该在两处被分到不同的档（比如这里"在看"、筛选条里"未观看"）。
fn favorite_tier(
    store: &crate::Store,
    user_id: domain::UserId,
    all_rows: &[domain::LedgerRow],
    media_id: domain::MediaId,
) -> WatchTier {
    let Some(media) = store.get_media(media_id).ok().flatten() else {
        return WatchTier::default();
    };
    let rows = all_rows.iter().filter(|row| row.media_id == media_id);
    match crate::http::library::selection::watch_state_for(store, user_id, &media, rows) {
        Ok(state) => WatchTier::of(state.seen, state.played),
        Err(error) => {
            tracing::warn!(%error, %media_id, "收藏行的观看状态读取失败，未看优先按未看处理");
            WatchTier::default()
        }
    }
}

/// 排序。`unwatched_first` 开着时先按观看分级（未看 → 在看 → 已看完），段内仍是这一档
/// 自己的排序；关掉时与加这个开关之前逐字相同。末位一律用 id 兜底，保证翻页不重不漏。
///
/// title/year 比较键在排序前一次物化，比较器内不再查库。
fn sort_favorite_entries(
    store: &crate::Store,
    list: &mut [FavoriteEntry],
    sort_key: &str,
    is_desc: bool,
    unwatched_first: bool,
) {
    let keys: HashMap<domain::MediaId, (String, Option<u16>)> = list
        .iter()
        .map(|entry| {
            let media = store.get_media(entry.media_id).ok().flatten();
            let title = media
                .as_ref()
                .map(|media| media.title.clone())
                .unwrap_or_default();
            let year = media.and_then(|media| media.year);
            (entry.media_id, (title, year))
        })
        .collect();
    let title = |id: &domain::MediaId| {
        keys.get(id)
            .map(|(title, _)| title.clone())
            .unwrap_or_default()
    };
    let year = |id: &domain::MediaId| keys.get(id).and_then(|(_, year)| *year);
    list.sort_by(|a, b| {
        let tier = if unwatched_first {
            a.tier.unwrap_or_default().cmp(&b.tier.unwrap_or_default())
        } else {
            std::cmp::Ordering::Equal
        };
        let primary = match sort_key {
            "title" if is_desc => title(&b.media_id).cmp(&title(&a.media_id)),
            "title" => title(&a.media_id).cmp(&title(&b.media_id)),
            "release_date" | "year" if is_desc => year(&b.media_id).cmp(&year(&a.media_id)),
            "release_date" | "year" => year(&a.media_id).cmp(&year(&b.media_id)),
            _ if is_desc => b.updated_at.cmp(&a.updated_at),
            _ => a.updated_at.cmp(&b.updated_at),
        };
        tier.then(primary)
            .then_with(|| a.media_id.to_string().cmp(&b.media_id.to_string()))
    });
}

