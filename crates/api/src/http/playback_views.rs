//! Home/playback views: up-next, favorites wall, favorites gallery.

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

use crate::http::library::poster_path;
use crate::http::{ok, ok_list};
use crate::management::ApiState;

/// 一条续看计划：阶段一锁内收集，阶段二锁外查季详情，阶段三锁内组装。
#[derive(Clone)]
struct UpNextPlan {
    media_id: domain::MediaId,
    tmdb_id: Option<String>,
    season: u32,
    next_episode: i32,
    /// 用户在本季看过（看完）至少一集 → 前端显示「看完上一集」。
    advanced: bool,
    /// 该用户对此媒体最后一次播放活动的 unix 秒；0 = 从未播放。
    last_activity: i64,
}

/// GET /playback/up-next?limit= — next unwatched unit per in-progress TV / Movie
/// subscription or watched local library items, with poster + resume position.
///
/// 全程按当前用户隔离：既包含用户播放历史（playback_units）中未看完的电影或
/// 已看集数的下一集（支持本地扫库作品），也包含用户的在追订阅。
/// 下一集只在**真的在库**（台账里有该集文件）时才展示，避免给出无法播放的卡片。
pub(crate) async fn up_next(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let limit = query
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20)
        .clamp(1, 100);
    // 阶段一：锁内只做快 DB 读，收集每条的 (media_id, season, next_episode)。
    let mut plans: Vec<UpNextPlan> = Vec::new();
    let mut seen_media: HashSet<domain::MediaId> = HashSet::new();

    {
        let store = state.store.lock();
        let ledger = store.list_ledger().unwrap_or_default();

        // 1. 优先读取当前用户的活跃播放记录 (playback_units)
        // 覆盖本地扫库作品和已有播放进度的条目（按最近活跃时间倒序）
        let active_media = store.user_active_media_ids(user_id).unwrap_or_default();
        for (media_id, last_activity) in active_media {
            if seen_media.contains(&media_id) {
                continue;
            }
            let Some(media) = store.get_media(media_id).ok().flatten() else {
                continue;
            };

            // 检查该媒体在当前用户的库中是否可见、是否有文件，且库未被设置为 exclude_from_home
            let media_rows: Vec<&domain::LedgerRow> = ledger
                .iter()
                .filter(|r| {
                    r.media_id == media_id
                        && crate::http::library::row_visible_to_user(
                            &store,
                            r,
                            &media,
                            Some(user_id),
                        )
                        && crate::http::playback::library_id_for(&store, &media, &[r])
                            .and_then(|lib_id| store.get_library(&lib_id).ok().flatten())
                            .map(|lib| !lib.exclude_from_home)
                            .unwrap_or(true)
                })
                .collect();
            if media_rows.is_empty() {
                continue;
            }

            let units = store.unit_rows(user_id, media.id).unwrap_or_default();

            if media.kind == domain::MediaKind::Movie || media.kind == domain::MediaKind::Video {
                // 电影 / 单片：如果已标记完全播放（played == true），则不需要在「接下来看」出现；
                // 只有看了一部分（!played && position_ms > 0）时进入「接下来看」
                let whole_unit = store.unit_state(user_id, media.id, -1, -1).ok().flatten();
                if let Some(ref u) = whole_unit {
                    if !u.played && u.position_ms > 0 {
                        seen_media.insert(media.id);
                        plans.push(UpNextPlan {
                            media_id: media.id,
                            tmdb_id: media.tmdb_id.clone(),
                            season: 0,
                            next_episode: 0,
                            advanced: false,
                            last_activity,
                        });
                    }
                }
            } else if media.kind == domain::MediaKind::Tv {
                // 电视剧：找出用户最后播放或最后标记看过的季和集
                // 查找该媒体库在位的所有集数 (season, episode)
                let owned_units: HashSet<(u32, u32)> = media_rows
                    .iter()
                    .filter_map(|r| Some((r.season?, r.episode?)))
                    .collect();

                // 用户最近更新过的播放记录
                let latest_unit = units.iter().max_by_key(|u| u.updated_at);
                if let Some(unit) = latest_unit {
                    let s = if unit.season >= 0 {
                        unit.season as u32
                    } else {
                        1
                    };
                    let e = if unit.episode >= 0 {
                        unit.episode as u32
                    } else {
                        1
                    };

                    let (target_s, target_e, is_advanced) = if !unit.played {
                        // 当前集还没看完，继续看当前集
                        (s, e, false)
                    } else {
                        // 当前集看完了，自动找下一集
                        let mut next_s = s;
                        let mut next_e = e + 1;
                        if !owned_units.contains(&(next_s, next_e)) {
                            // 尝试下一季第 1 集
                            if owned_units.contains(&(next_s + 1, 1)) {
                                next_s += 1;
                                next_e = 1;
                            }
                        }
                        (next_s, next_e, true)
                    };

                    if owned_units.contains(&(target_s, target_e)) {
                        seen_media.insert(media.id);
                        plans.push(UpNextPlan {
                            media_id: media.id,
                            tmdb_id: media.tmdb_id.clone(),
                            season: target_s,
                            next_episode: target_e as i32,
                            advanced: is_advanced,
                            last_activity,
                        });
                    }
                }
            }

            if plans.len() >= limit * 2 {
                break;
            }
        }

        // 2. 补充尚未开播、但在追的电视剧订阅 (Coverage::Tv)
        for subscribe in store.list_subscribes_for_user(user_id).unwrap_or_default() {
            if seen_media.contains(&subscribe.media_id) {
                continue;
            }
            let Some(media) = store.get_media(subscribe.media_id).ok().flatten() else {
                continue;
            };
            let domain::Coverage::Tv {
                season,
                episode_from,
                episode_to,
            } = subscribe.coverage
            else {
                continue;
            };
            let units = store.unit_rows(user_id, media.id).unwrap_or_default();
            let last_played = units
                .iter()
                .filter(|u| u.season == season as i32 && u.episode >= 0 && u.played)
                .map(|u| u.episode)
                .max();
            let last_activity = units.iter().map(|u| u.updated_at).max().unwrap_or(0);
            let start_from = last_played.map(|e| e + 1).unwrap_or(episode_from as i32);
            let owned: HashSet<u32> = ledger
                .iter()
                .filter(|r| {
                    r.media_id == media.id
                        && r.season == Some(season)
                        && crate::http::library::row_visible_to_user(
                            &store,
                            r,
                            &media,
                            Some(user_id),
                        )
                        && crate::http::playback::library_id_for(&store, &media, &[r])
                            .and_then(|lib_id| store.get_library(&lib_id).ok().flatten())
                            .map(|lib| !lib.exclude_from_home)
                            .unwrap_or(true)
                })
                .filter_map(|r| r.episode)
                .collect();
            let cap = start_from.saturating_add(domain::Coverage::MAX_EPISODES as i32 - 1);
            let window_to = owned
                .iter()
                .copied()
                .max()
                .map(|m| m as i32)
                .unwrap_or_else(|| episode_to.map(|t| t as i32).unwrap_or(start_from))
                .min(cap)
                .max(start_from);
            let Some(next_episode) =
                (start_from..=window_to).find(|ep| owned.contains(&(*ep as u32)))
            else {
                continue;
            };
            seen_media.insert(media.id);
            plans.push(UpNextPlan {
                media_id: media.id,
                tmdb_id: media.tmdb_id.clone(),
                season,
                next_episode,
                advanced: last_played.is_some(),
                last_activity,
            });
            if plans.len() >= limit * 2 {
                break;
            }
        }
    }

    // 阶段二：锁外批量预取季详情（同步 ureq 挪到 spawn_blocking）。
    let state_clone = state.clone();
    let plans_clone = plans.clone();
    let titles: std::collections::HashMap<(String, u32, i32), Option<String>> =
        tokio::task::spawn_blocking(move || {
            let catalog: &dyn crate::catalog::Catalog = state_clone.catalog.as_ref();
            let mut out = std::collections::HashMap::new();
            for plan in plans_clone {
                if let Some(ref tmdb_id) = plan.tmdb_id {
                    let title = catalog
                        .season_details(tmdb_id, plan.season)
                        .unwrap_or_default()
                        .into_iter()
                        .find(|m| m.episode_number == plan.next_episode as u32)
                        .and_then(|m| m.name);
                    out.insert((tmdb_id.clone(), plan.season, plan.next_episode), title);
                }
            }
            out
        })
        .await
        .unwrap_or_default();

    // 阶段三：锁内组装（只查 HashMap，无网络）。
    let mut items = Vec::new();
    {
        let store = state.store.lock();
        let all_rows = store.list_ledger().unwrap_or_default();
        for plan in plans {
            let Some(media) = store.get_media(plan.media_id).ok().flatten() else {
                continue;
            };
            let episode_title = plan
                .tmdb_id
                .as_ref()
                .and_then(|tmdb_id| titles.get(&(tmdb_id.clone(), plan.season, plan.next_episode)))
                .cloned()
                .flatten();
            let is_movie =
                media.kind == domain::MediaKind::Movie || media.kind == domain::MediaKind::Video;
            let (target_season, target_episode) = if is_movie {
                (-1, -1)
            } else {
                (plan.season as i32, plan.next_episode)
            };
            let mut unit = store
                .unit_state(user_id, media.id, target_season, target_episode)
                .ok()
                .flatten();
            if unit.is_none() && is_movie {
                unit = store.unit_state(user_id, media.id, 0, 0).ok().flatten();
            }
            let rows: Vec<&domain::LedgerRow> = all_rows
                .iter()
                .filter(|r| r.media_id == media.id)
                .filter(|r| {
                    crate::http::library::row_visible_to_user(&store, r, &media, Some(user_id))
                })
                .collect();
            if rows.is_empty() {
                continue;
            }
            let library_id = rows
                .first()
                .and_then(|row| crate::http::library::library_for_row(&store, row, &media))
                .map(|library| library.id)
                .unwrap_or_default();
            let poster = crate::http::library::preferred_row(&rows)
                .and_then(poster_path)
                .map(|_| {
                    let row = crate::http::library::preferred_row(&rows).expect("checked");
                    crate::http::library::artwork_url("posters", row.id)
                });
            let mut backdrop = crate::http::library::preferred_row(&rows)
                .and_then(crate::http::library::backdrop_path)
                .map(|_| {
                    let row = crate::http::library::preferred_row(&rows).expect("checked");
                    crate::http::library::artwork_url("fanart", row.id)
                });
            // 锁外异步拉图或本地兜底：若本地尚无 fanart.jpg，先不阻塞 GET /up-next
            if backdrop.is_none() {
                if let Some(preferred) = crate::http::library::preferred_row(&rows) {
                    let state_clone = state.clone();
                    let media_clone = media.clone();
                    let pref_path = std::path::PathBuf::from(&preferred.path);
                    tokio::spawn(async move {
                        let _ = crate::poster_fetch::attach_backdrop(
                            &state_clone,
                            &media_clone,
                            &pref_path,
                        );
                    });
                }
            }
            // 剧照：优先读取下一集自己的剧照；没有该图就回退到背景图，若背景图还没有则回退海报
            let next_row = if media.kind == domain::MediaKind::Tv {
                rows.iter()
                    .find(|r| {
                        r.season == Some(plan.season) && r.episode == Some(plan.next_episode as u32)
                    })
                    .copied()
            } else {
                rows.first().copied()
            };
            let still = next_row
                .and_then(|row| {
                    crate::episode_still::existing(std::path::Path::new(&row.path))
                        .map(|_| crate::http::library::artwork_url("stills", row.id))
                })
                .or_else(|| backdrop.clone())
                .or_else(|| {
                    crate::http::library::preferred_row(&rows)
                        .map(|row| crate::http::library::artwork_url("fanart", row.id))
                });
            if backdrop.is_none() {
                backdrop = crate::http::library::preferred_row(&rows)
                    .map(|row| crate::http::library::artwork_url("fanart", row.id));
            }
            let position = unit.as_ref().map(|u| u.position_ms).unwrap_or(0);
            let duration = unit.as_ref().and_then(|u| u.duration_ms);
            let progress = duration
                .filter(|d| *d > 0)
                .map(|d| ((position as f64 / d as f64) * 100.0).round() as i64)
                .filter(|p| (0..=100).contains(p));
            // 从未播放过就给空串：前端显示「开始观看」，不伪造「刚刚打开过」。
            let last_played_at = if plan.last_activity > 0 {
                millis(plan.last_activity)
            } else {
                String::new()
            };
            items.push(json!({
                "media_item_id": media.id.to_string(),
                "library_id": library_id,
                "kind": media.kind.as_str(),
                "title": media.title,
                "year": media.year,
                "poster_url": poster,
                "poster_aspect": 0.667,
                "backdrop_url": backdrop,
                "episode_still_url": still,
                "season_number": plan.season,
                "episode_number": plan.next_episode,
                "episode_title": episode_title,
                "unwatched_ahead_count": 0,
                "position_ms": position,
                "duration_ms": duration,
                "progress_percent": progress,
                "advanced": plan.advanced,
                "last_played_at": last_played_at,
            }));
            if items.len() >= limit {
                break;
            }
        }
    }
    ok(json!({ "items": items })).into_response()
}

fn millis(unix_secs: i64) -> String {
    (unix_secs as i128 * 1000).to_string()
}

/// 收藏行的一条：作品 + 收藏所在的层级（最近一次收藏动作的单元）。
struct FavoriteEntry {
    media_id: domain::MediaId,
    /// 最近一次收藏动作的时间（默认档按它排）
    updated_at: i64,
    season: i32,
    episode: i32,
    /// 未看优先才填：未看 → 在看 → 已看完
    tier: Option<library::WatchTier>,
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
    let store = state.store.lock();
    let all_rows = store.list_ledger().unwrap_or_default();
    let mut list = favorite_entries(&store, user_id, &all_rows, unwatched_first);
    sort_favorite_entries(&store, &mut list, sort_key, is_desc, unwatched_first);
    let total = list.len();
    let page: Vec<Value> = list
        .into_iter()
        .skip(offset)
        .take(limit)
        .filter_map(|entry| favorite_item_json(&store, &all_rows, &entry, user_id))
        .collect();
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
) -> library::WatchTier {
    let Some(media) = store.get_media(media_id).ok().flatten() else {
        return library::WatchTier::default();
    };
    let rows = all_rows.iter().filter(|row| row.media_id == media_id);
    match crate::http::library::selection::watch_state_for(store, user_id, &media, rows) {
        Ok(state) => library::WatchTier::of(state.seen, state.played),
        Err(error) => {
            tracing::warn!(%error, %media_id, "收藏行的观看状态读取失败，未看优先按未看处理");
            library::WatchTier::default()
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

/// 一格收藏卡：与单库页库存格同形，另加「收藏在哪一层」。
fn favorite_item_json(
    store: &crate::Store,
    all_rows: &[domain::LedgerRow],
    entry: &FavoriteEntry,
    user_id: domain::UserId,
) -> Option<Value> {
    let media_id = entry.media_id;
    let media = store.get_media(media_id).ok().flatten()?;
    let owned: Vec<domain::LedgerRow> = all_rows
        .iter()
        .filter(|r| r.media_id == media_id)
        .filter(|r| crate::http::library::row_visible_to_user(store, r, &media, Some(user_id)))
        .cloned()
        .collect();
    if owned.is_empty() {
        return None;
    }
    let refs: Vec<&domain::LedgerRow> = owned.iter().collect();
    let library_id = refs
        .first()
        .and_then(|row| crate::http::library::library_for_row(store, row, &media))
        .map(|library| library.id)
        .unwrap_or_default();
    let poster = crate::http::library::preferred_row(&refs)
        .and_then(poster_path)
        .map(|_| {
            let row = crate::http::library::preferred_row(&refs).expect("checked");
            crate::http::library::artwork_url("posters", row.id)
        });
    Some(json!({
        "media_item_id": media.id.to_string(),
        "kind": media.kind.as_str(),
        "library_id": library_id,
        "title": media.title,
        "year": media.year,
        "poster_url": poster,
        "file_count": owned.len(),
        "seasons": owned.iter().filter_map(|r| r.season).collect::<std::collections::BTreeSet<_>>(),
        "episode_count": owned.iter().filter(|r| r.episode.is_some()).count() as i64,
        "favorite_season_number": (entry.season >= 0).then_some(entry.season),
        "favorite_episode_number": (entry.episode >= 0).then_some(entry.episode),
    }))
}

/// GET /playback/favorites/gallery — 收藏图廊：每部作品的海报/背景/剧照，
/// 形状对齐前端 LibraryGalleryGroup（listFavoritesGallery 直读）。
pub(crate) async fn favorites_gallery(
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
    let store = state.store.lock();
    let all_rows = store.list_ledger().unwrap_or_default();
    let media_ids: HashSet<domain::MediaId> = all_rows
        .iter()
        .filter_map(|row| {
            let media = store.get_media(row.media_id).ok().flatten()?;
            crate::http::library::row_visible_to_user(&store, row, &media, Some(user_id))
                .then_some(row.media_id)
        })
        .collect();
    let mut by_media: HashMap<domain::MediaId, i64> = HashMap::new();
    for media_id in media_ids {
        for row in store.unit_rows(user_id, media_id).unwrap_or_default() {
            if row.favorite {
                by_media.insert(media_id, row.updated_at);
            }
        }
    }
    let sort_key = query
        .get("sort")
        .map(String::as_str)
        .unwrap_or("updated_at");
    let is_desc = query
        .get("order")
        .map(|o| o == "desc")
        .unwrap_or_else(|| sort_key != "title");
    let mut list: Vec<(domain::MediaId, i64)> = by_media.into_iter().collect();
    if sort_key == "title" {
        list.sort_by(|(a_id, _), (b_id, _)| {
            let a_title = store
                .get_media(*a_id)
                .ok()
                .flatten()
                .map(|m| m.title)
                .unwrap_or_default();
            let b_title = store
                .get_media(*b_id)
                .ok()
                .flatten()
                .map(|m| m.title)
                .unwrap_or_default();
            if is_desc {
                b_title.cmp(&a_title)
            } else {
                a_title.cmp(&b_title)
            }
        });
    } else {
        list.sort_by(|(_, a_up), (_, b_up)| {
            if is_desc {
                b_up.cmp(a_up)
            } else {
                a_up.cmp(b_up)
            }
        });
    }
    let groups: Vec<Value> = list
        .into_iter()
        .skip(offset)
        .take(limit)
        .filter_map(|(media_id, _)| {
            let media = store.get_media(media_id).ok().flatten()?;
            let owned: Vec<domain::LedgerRow> = all_rows
                .iter()
                .filter(|r| r.media_id == media_id)
                .filter(|r| {
                    crate::http::library::row_visible_to_user(&store, r, &media, Some(user_id))
                })
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
            let mut images = Vec::new();
            if let Some(row) = crate::http::library::preferred_row(&refs) {
                let dir = std::path::Path::new(&row.path)
                    .parent()
                    .map(|p| p.to_path_buf());
                if let Some(dir) = dir {
                    if dir.join("poster.jpg").is_file() {
                        images.push(json!({
                            "kind": "poster", "url": crate::http::library::artwork_url("posters", row.id),
                            "aspect": 0.667, "label": "海报",
                            "season": null, "episode": null, "t_seconds": null,
                        }));
                    }
                    if dir.join("fanart.jpg").is_file() {
                        images.push(json!({
                            "kind": "backdrop", "url": crate::http::library::artwork_url("fanart", row.id),
                            "aspect": 1.778, "label": "背景",
                            "season": null, "episode": null, "t_seconds": null,
                        }));
                    }
                    for row in owned.iter() {
                        if row.episode.is_some()
                            && crate::episode_still::existing(std::path::Path::new(&row.path)).is_some()
                        {
                            images.push(json!({
                                "kind": "still",
                                "url": crate::http::library::artwork_url("stills", row.id),
                                "aspect": 1.778, "label": "剧照",
                                "season": row.season, "episode": row.episode, "t_seconds": null,
                            }));
                        }
                    }
                }
            }
            Some(json!({
                "media_item_id": media.id.to_string(),
                "library_id": library_id,
                "kind": media.kind.as_str(),
                "title": media.title,
                "year": media.year,
                "is_favorite": true,
                "images": images,
            }))
        })
        .collect();
    ok_list(groups).into_response()
}
