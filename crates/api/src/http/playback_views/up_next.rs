use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

use crate::http::library::poster_path;
use crate::http::ok;
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
                        // 当前集看完了，找下一集
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

    // 阶段三：锁内物化行与状态（纯 DB 读，无 IO）。
    struct UpNextCandidate {
        media: domain::Media,
        library_id: String,
        preferred_row: Option<domain::LedgerRow>,
        next_row: Option<domain::LedgerRow>,
        plan_season: u32,
        plan_next_episode: i32,
        episode_title: Option<String>,
        position: i64,
        duration: Option<i64>,
        progress: Option<i64>,
        advanced: bool,
        last_played_at: String,
    }

    let candidates: Vec<UpNextCandidate> = {
        let store = state.store.lock();
        let all_rows = store.list_ledger().unwrap_or_default();
        let mut cands = Vec::new();
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
            let rows: Vec<domain::LedgerRow> = all_rows
                .iter()
                .filter(|r| r.media_id == media.id)
                .filter(|r| {
                    crate::http::library::row_visible_to_user(&store, r, &media, Some(user_id))
                })
                .cloned()
                .collect();
            if rows.is_empty() {
                continue;
            }
            let library_id = rows
                .first()
                .and_then(|row| crate::http::library::library_for_row(&store, row, &media))
                .map(|library| library.id)
                .unwrap_or_default();

            let refs: Vec<&domain::LedgerRow> = rows.iter().collect();
            let preferred_row = crate::http::library::preferred_row(&refs).cloned();

            let next_row = if media.kind == domain::MediaKind::Tv {
                rows.iter()
                    .find(|r| {
                        r.season == Some(plan.season) && r.episode == Some(plan.next_episode as u32)
                    })
                    .cloned()
            } else {
                rows.first().cloned()
            };

            let position = unit.as_ref().map(|u| u.position_ms).unwrap_or(0);
            let duration = unit.as_ref().and_then(|u| u.duration_ms);
            let progress = duration
                .filter(|d| *d > 0)
                .map(|d| ((position as f64 / d as f64) * 100.0).round() as i64)
                .filter(|p| (0..=100).contains(p));
            let last_played_at = if plan.last_activity > 0 {
                millis(plan.last_activity)
            } else {
                String::new()
            };

            cands.push(UpNextCandidate {
                media,
                library_id,
                preferred_row,
                next_row,
                plan_season: plan.season,
                plan_next_episode: plan.next_episode,
                episode_title,
                position,
                duration,
                progress,
                advanced: plan.advanced,
                last_played_at,
            });
            if cands.len() >= limit {
                break;
            }
        }
        cands
    };

    // 阶段四：有界 blocking 观测 artwork
    let files = state.browse_files.clone();
    let state_for_work = state.clone();
    let items_res = state
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
                let mut backdrop = cand
                    .preferred_row
                    .as_ref()
                    .and_then(|row| crate::http::library::backdrop_path_with(row, files.as_ref()))
                    .map(|_| {
                        let row = cand.preferred_row.as_ref().expect("checked");
                        crate::http::library::artwork_url("fanart", row.id)
                    });
                if backdrop.is_none() {
                    if let Some(preferred) = &cand.preferred_row {
                        let state_clone = state_for_work.clone();
                        let media_clone = cand.media.clone();
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
                let still = cand
                    .next_row
                    .as_ref()
                    .and_then(|row| {
                        crate::episode_still::existing_with(std::path::Path::new(&row.path), files.as_ref())
                            .map(|_| crate::http::library::artwork_url("stills", row.id))
                    })
                    .or_else(|| backdrop.clone())
                    .or_else(|| {
                        cand.preferred_row
                            .as_ref()
                            .map(|row| crate::http::library::artwork_url("fanart", row.id))
                    });
                if backdrop.is_none() {
                    backdrop = cand
                        .preferred_row
                        .as_ref()
                        .map(|row| crate::http::library::artwork_url("fanart", row.id));
                }

                items.push(json!({
                    "media_item_id": cand.media.id.to_string(),
                    "library_id": cand.library_id,
                    "kind": cand.media.kind.as_str(),
                    "title": cand.media.title,
                    "year": cand.media.year,
                    "poster_url": poster,
                    "poster_aspect": 0.667,
                    "backdrop_url": backdrop,
                    "episode_still_url": still,
                    "season_number": cand.plan_season,
                    "episode_number": cand.plan_next_episode,
                    "episode_title": cand.episode_title,
                    "unwatched_ahead_count": 0,
                    "position_ms": cand.position,
                    "duration_ms": cand.duration,
                    "progress_percent": cand.progress,
                    "advanced": cand.advanced,
                    "last_played_at": cand.last_played_at,
                }));
            }
            items
        })
        .await;

    let items = match items_res {
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
                "up_next.timeout",
                "待播列表加载超时",
            );
        }
        Err(crate::blocking_budget::BlockingError::Worker(msg)) => {
            return crate::http::err(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "up_next.worker",
                &msg,
            );
        }
    };
    ok(json!({ "items": items })).into_response()
}

fn millis(unix_secs: i64) -> String {
    (unix_secs as i128 * 1000).to_string()
}
