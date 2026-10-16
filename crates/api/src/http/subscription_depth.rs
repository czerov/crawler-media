//! Subscription depth endpoints (CONTEXT.md **Subscribe** depth): wash-cut
//! upgrade reports, removal preview, season cleanup, activity timeline, and
//! today-arrivals. Split out of `subscriptions.rs` (CRUD + JSON) to keep both
//! files under the repo size budget.

use std::str::FromStr;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::Subscribe;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::http::{err, ok};
use crate::management::ApiState;

mod activity;
mod arrivals;
mod forecast;

pub(crate) use activity::activities;
pub(crate) use arrivals::today_arrivals;

use super::subscriptions::{remove_torrents_from_client, resolve_subscribe, user_is_admin};

#[derive(Deserialize, Default)]
pub(crate) struct RunUpgradeInput {
    #[serde(default)]
    pub(crate) rule_set_id: Option<String>,
    #[serde(default)]
    pub(crate) filter_id: Option<String>,
    #[serde(default)]
    pub(crate) wash_cut: Option<bool>,
}

pub(crate) async fn run_upgrade(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
    body_bytes: axum::body::Bytes,
) -> Response {
    let (mut subscribe, media) = match resolve_subscribe(&state, &id, user_id) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let (input, has_body) = if body_bytes.is_empty() || body_bytes.as_ref() == b"null" {
        (RunUpgradeInput::default(), false)
    } else {
        (
            serde_json::from_slice(&body_bytes).unwrap_or_default(),
            true,
        )
    };
    let new_filter_str = input.rule_set_id.or(input.filter_id);
    let mut dirty = false;
    if let Some(fid_str) = new_filter_str {
        if let Ok(fid) = domain::FilterId::from_str(&fid_str) {
            let filter_exists = state.store.lock().get_filter(fid).ok().flatten().is_some();
            if filter_exists {
                subscribe.wash_cut_filter_id = Some(fid);
                dirty = true;
            }
        }
    }
    if has_body && input.wash_cut.unwrap_or(true) && !subscribe.wash_cut {
        subscribe.wash_cut = true;
        dirty = true;
    }
    if dirty {
        let _ = state.store.lock().update_subscribe(&subscribe);
    }

    let store = state.store.lock();
    let (target_score, target_label) = wash_target(&store, &subscribe);
    let facts = store.load_subscribe_facts(subscribe.id).unwrap_or_default();
    let pending = store.load_pending(subscribe.id).unwrap_or_default();
    let history = store.load_wanted_history(subscribe.id).unwrap_or_default();

    let mut units: Vec<Value> = Vec::new();
    let mut counts: std::collections::HashMap<&str, i64> = [
        ("upgradable", 0),
        ("at_cutoff", 0),
        ("in_flight", 0),
        ("not_comparable", 0),
        ("missing", 0),
    ]
    .into_iter()
    .collect();
    for (season, episode) in
        crate::open_coverage::units(&subscribe, &media, &facts, &pending, &history, [])
    {
        let fact = facts.get(season, episode);
        let in_flight = pending
            .iter()
            .any(|(_, p)| release_covers_unit(&p.torrent.title, season, episode));
        let state = unit_wash_state(&subscribe, fact, in_flight, target_score);
        *counts.get_mut(state).unwrap() += 1;
        units.push(json!({
            "season_number": season.unwrap_or(0) as i64,
            "episode_number": episode.unwrap_or(0) as i64,
            "state": state,
            "current_label": null,
            "target_label": if target_label.is_empty() { Value::Null } else { json!(target_label) },
        }));
    }
    let summary = upgrade_summary(&counts, &target_label, subscribe.wash_cut);
    let counts = json!({
        "upgradable": counts["upgradable"],
        "at_cutoff": counts["at_cutoff"],
        "in_flight": counts["in_flight"],
        "not_comparable": counts["not_comparable"],
        "missing": counts["missing"],
    });
    drop(store);
    // 可升级/缺失的单元排入立即搜索；其余保持现状。
    let queued_upgrade = counts["upgradable"].as_i64().unwrap_or(0) > 0;
    let queued_missing = counts["missing"].as_i64().unwrap_or(0) > 0;
    if queued_upgrade || queued_missing {
        if let Err(error) = crate::jobs_api::seed_subscribe_search(
            &state.jobs.lock(),
            &subscribe.id.to_string(),
            Some(&media.title),
        ) {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "job.seed_failed",
                &error.to_string(),
            );
        }
    }
    ok(json!({
        "summary": summary,
        "target_label": target_label,
        "rule_set_id": subscribe.wash_cut_filter_id.or(Some(subscribe.filter_id)).map(|id| id.to_string()),
        "counts": counts,
        "units": units,
    }))
    .into_response()
}

/// 洗版目标：(最高优先级原子分数, 人类标签)。来源 = wash_cut_filter_id（缺省
/// 复用 filter_id）；未配置任何原子时目标分数为 0（无法判定）。
pub(crate) fn wash_target(store: &crate::Store, subscribe: &Subscribe) -> (i32, String) {
    let filter_id = subscribe.wash_cut_filter_id.or(Some(subscribe.filter_id));
    let filter = filter_id.and_then(|fid| store.get_filter(fid).ok().flatten());
    // 显式 WashTarget 原子优先（规则面板的「洗到哪一档」）；否则从 resolution/source 原子推导。
    if let Some(filter) = &filter {
        if let Some(target) = filter
            .atoms
            .iter()
            .filter_map(|a| match &a.rule {
                domain::AtomRule::WashTarget(v) => Some((v.clone(), a.priority)),
                _ => None,
            })
            .max_by_key(|(_, priority)| *priority)
        {
            return (target.1, target.0);
        }
    }
    let label = filter.as_ref().map(filter_label).unwrap_or_default();
    let score = filter
        .as_ref()
        .map(|f| f.atoms.iter().map(|a| a.priority).max().unwrap_or(0))
        .unwrap_or(0);
    (score, label)
}

/// 一个单元的洗版状态：in_flight / missing / at_cutoff / not_comparable / upgradable。
pub(crate) fn unit_wash_state(
    subscribe: &Subscribe,
    fact: Option<&subscribe::QualityFact>,
    in_flight: bool,
    target_score: i32,
) -> &'static str {
    if !subscribe.wash_cut {
        if fact.is_none() {
            "missing"
        } else {
            "at_cutoff"
        }
    } else if fact.is_none() {
        "missing"
    } else if in_flight {
        "in_flight"
    } else if target_score == 0 {
        "not_comparable"
    } else if fact.map(|f| f.score).unwrap_or(0) >= target_score {
        "at_cutoff"
    } else {
        "upgradable"
    }
}

/// 一个种子标题（Release 解析后）是否覆盖给定单元。
pub(crate) fn release_covers_unit(title: &str, season: Option<u32>, episode: Option<u32>) -> bool {
    let r = release::parse(title);
    match r.season {
        Some(s) if season == Some(s) => match (r.episode, episode) {
            (Some(_from), Some(ep)) => r
                .episode_span()
                .is_some_and(|(from, to)| ep >= from && ep <= to),
            (Some(_), None) => false,
            (None, None) => true,
            (None, Some(_)) => false,
        },
        Some(_) => false,
        None => season.is_none() && episode.is_none(),
    }
}

/// 详情接口的 wanted 列表：单元状态按 facts（imported）/ pending（grabbed）
/// 现算，履历（search_attempts / grab / reject / imported_at）读工单历史表。
pub(crate) fn wanted_json(
    store: &crate::Store,
    catalog: &dyn crate::catalog::Catalog,
    media: &domain::Media,
    subscribe: &Subscribe,
    facts: &subscribe::SubscribeFacts,
) -> Vec<Value> {
    let air_dates: std::collections::HashMap<u32, Option<String>> =
        match (media.kind, media.tmdb_id.as_deref(), &subscribe.coverage) {
            (domain::MediaKind::Tv, Some(tmdb_id), domain::Coverage::Tv { season, .. }) => catalog
                .season_episodes(tmdb_id, *season)
                .map(|episodes| {
                    episodes
                        .into_iter()
                        .map(|ep| (ep.episode_number, ep.air_date))
                        .collect()
                })
                .unwrap_or_default(),
            _ => std::collections::HashMap::new(),
        };
    wanted_json_with_air_dates(store, media, subscribe, facts, air_dates)
}

pub(crate) fn wanted_json_with_air_dates(
    store: &crate::Store,
    media: &domain::Media,
    subscribe: &Subscribe,
    facts: &subscribe::SubscribeFacts,
    air_dates: std::collections::HashMap<u32, Option<String>>,
) -> Vec<Value> {
    let pending = store.load_pending(subscribe.id).unwrap_or_default();
    let history = store.load_wanted_history(subscribe.id).unwrap_or_default();
    let (target_score, target_label) = wash_target(store, subscribe);
    let ledger = store.list_ledger().unwrap_or_default();
    let fmt = crate::store::rfc3339_from_secs;
    crate::open_coverage::units(subscribe, media, facts, &pending, &history, air_dates.keys().copied())
        .into_iter()
        .map(|(season, episode)| {
            let fact = facts.get(season, episode);
            let in_flight = pending
                .iter()
                .any(|(_, p)| release_covers_unit(&p.torrent.title, season, episode));
            let h = history
                .get(&(season, episode))
                .cloned()
                .unwrap_or_default();
            let status = if fact.is_some() {
                "imported"
            } else if in_flight || h.grabbed_at.is_some() {
                "grabbed"
            } else {
                "wanted"
            };
            let state = unit_wash_state(subscribe, fact, in_flight, target_score);
            let current_label = ledger
                .iter()
                .find(|row| {
                    row.media_id == subscribe.media_id
                        && row.season == season
                        && row.episode == episode
                })
                .and_then(|row| match (row.resolution.as_deref(), row.codec.as_deref()) {
                    (Some(r), Some(c)) => Some(format!("{r} {c}")),
                    (Some(r), None) => Some(r.to_string()),
                    (None, Some(c)) => Some(c.to_string()),
                    (None, None) => None,
                });
            let upgrade = if status == "wanted" {
                Value::Null
            } else {
                json!({
                    "active": state == "upgradable" || state == "in_flight",
                    "current_label": current_label,
                    "target_label": if target_label.is_empty() { Value::Null } else { json!(target_label) },
                    "search_attempts": h.search_attempts,
                    "indeterminate": state == "not_comparable",
                })
            };
            let air_date = match (media.kind, episode) {
                (domain::MediaKind::Tv, Some(ep)) => air_dates.get(&ep).cloned().flatten(),
                _ => None,
            };
            let release_forecast = air_date
                .as_deref()
                .and_then(|date| forecast::forecast_for_unit(subscribe, &pending, date))
                .unwrap_or(Value::Null);
            let resource_timing = if status == "grabbed" {
                pending
                    .iter()
                    .find(|(_, p)| release_covers_unit(&p.torrent.title, season, episode))
                    .map(|(_, p)| {
                        let publish_unix = p
                            .torrent
                            .upload_time
                            .as_deref()
                            .and_then(super::subscriptions::parse_upload_time);
                        let submitted = p.submitted_at;
                        json!({
                            "site_id": p.torrent.site_id.to_string(),
                            "torrent_id": p.torrent.id.clone(),
                            "publish_time": p.torrent.upload_time,
                            "first_seen_at": submitted.map(fmt),
                            "submitted_at": submitted.map(fmt),
                            "publish_to_seen_seconds": Value::Null,
                            "seen_to_submit_seconds": Value::Null,
                            "publish_to_submit_seconds": match (publish_unix, submitted) {
                                (Some(publish), Some(submit)) if submit >= publish => {
                                    json!(submit - publish)
                                }
                                _ => Value::Null,
                            },
                            "dry_run": false,
                        })
                    })
                    .unwrap_or(Value::Null)
            } else {
                Value::Null
            };
            let now = crate::job_loop::unix_now();
            let next_search = if status == "wanted" {
                match &air_date {
                    Some(date_str) => {
                        let today_str = crate::store::now_rfc3339();
                        let today = today_str.split('T').next().unwrap_or(&today_str);
                        if date_str.as_str() > today {
                            // 播出日在未来：到期日 = 播出日（按当天 0 点估算 unix 时间戳）
                            crate::http::subscriptions::parse_upload_time(date_str)
                        } else {
                            h.last_search_at
                                .map(|ts| ts + subscribe.search_interval_secs as i64)
                                .or(Some(now))
                        }
                    }
                    None => {
                        // 未定档但处于订阅周期搜索中：下次搜索 = 上次搜索 + 搜索间隔
                        h.last_search_at
                            .map(|ts| ts + subscribe.search_interval_secs as i64)
                            .or(Some(now))
                    }
                }
            } else {
                None
            };
            json!({
                "id": format!("{}-{}-{}", subscribe.id, season.unwrap_or(0), episode.unwrap_or(0)),
                "season_number": season.unwrap_or(0) as i64,
                "episode_number": episode.unwrap_or(0) as i64,
                "status": status,
                "air_date": air_date,
                "priority": 0,
                "next_search_at": next_search.map(fmt),
                "search_attempts": h.search_attempts,
                "last_search_at": h.last_search_at.map(fmt),
                "release_forecast": release_forecast,
                "resource_timing": resource_timing,
                "grabbed_at": h.grabbed_at.map(fmt),
                "downloaded_at": Value::Null,
                "imported_at": h.imported_at.map(fmt),
                "info_hash": Value::Null,
                "last_reject_reason": h.last_reject_reason,
                "grab_title": h.grab_title,
                "upgrade": upgrade,
            })
        })
        .collect()
}

fn upgrade_summary(
    counts: &std::collections::HashMap<&str, i64>,
    target_label: &str,
    wash_cut: bool,
) -> String {
    let missing = counts["missing"];
    let upgradable = counts["upgradable"];
    let at_cutoff = counts["at_cutoff"];
    if !wash_cut {
        return format!(
            "共 {} 个单元：{} 个已入库，{} 个缺失；未开启洗版，无可升级单元",
            missing + at_cutoff + upgradable,
            at_cutoff,
            missing
        );
    }
    if target_label.is_empty() {
        return format!(
            "共 {} 个单元：{} 个缺失，{} 个已入库；规则组未配置洗版目标，无法判定可升级",
            missing + at_cutoff + upgradable,
            missing,
            at_cutoff
        );
    }
    format!(
        "共 {} 个单元：{} 个缺失，{} 个可升级至 {}，{} 个已达目标",
        missing + at_cutoff + upgradable,
        missing,
        upgradable,
        target_label,
        at_cutoff
    )
}

/// Human label for a Filter: highest-priority resolution + source atoms, e.g. `2160p Remux`.
fn filter_label(filter: &domain::Filter) -> String {
    let mut resolution = None;
    let mut source = None;
    let mut best = i32::MIN;
    for atom in &filter.atoms {
        if atom.priority > best {
            best = atom.priority;
            resolution = None;
            source = None;
        }
        if atom.priority == best {
            match &atom.rule {
                domain::AtomRule::Resolution(v) => resolution = Some(v.as_str()),
                domain::AtomRule::Source(v) => source = Some(v.as_str()),
                _ => {}
            }
        }
    }
    match (resolution, source) {
        (Some(r), Some(s)) => format!("{r} {s}"),
        (Some(r), None) => r.to_string(),
        (None, Some(s)) => s.to_string(),
        (None, None) => String::new(),
    }
}

pub(crate) async fn removal_preview(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
) -> Response {
    let (subscribe, media) = match resolve_subscribe(&state, &id, user_id) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let store = state.store.lock();
    let ledger = store.list_ledger().unwrap_or_default();
    let files: Vec<&domain::LedgerRow> = ledger
        .iter()
        .filter(|row| row.media_id == subscribe.media_id)
        .collect();
    let bytes: u64 = files.iter().map(|row| row_bytes(&row.path)).sum();
    // In-flight torrents this Subscribe is still waiting on.
    let pending = store.load_pending(subscribe.id).unwrap_or_default();
    let mut torrent_titles: Vec<String> = pending
        .iter()
        .map(|(_, p)| p.torrent.title.clone())
        .collect();

    // 如果 pending 中由于误判等原因已空，尝试从下载器快照中兜底匹配属于本订阅媒体的任务
    // D-L3: such matches are inferred by title only and flagged as such.
    let torrents_inferred = torrent_titles.is_empty();
    if torrents_inferred {
        drop(store);
        match state.downloader.task_snapshots() {
            Ok(snapshots) => {
                tracing::info!(
                    snapshots_len = snapshots.len(),
                    "检查下载器快照以兜底匹配订阅任务"
                );
                for s in snapshots {
                    tracing::debug!(snapshot_name = %s.name, snapshot_tag = %s.tag, "下载器快照");
                    if !s.tag.is_empty()
                        && (downloader::names_match(&s.name, &media.title)
                            || media
                                .original_title
                                .as_deref()
                                .is_some_and(|orig| downloader::names_match(&s.name, orig)))
                    {
                        torrent_titles.push(s.name);
                    }
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "获取下载器任务快照失败");
            }
        }
    } else {
        drop(store);
    }

    let torrent_count = torrent_titles.len();
    ok(json!({
        "library_file_count": files.len(),
        "library_bytes": bytes,
        "torrent_count": torrent_count,
        "torrent_count_inferred": torrents_inferred && torrent_count > 0,
        "torrent_titles": torrent_titles,
    }))
    .into_response()
}

fn row_bytes(path: &str) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

#[derive(Deserialize)]

pub(crate) struct SeasonCleanupInput {
    #[serde(default)]
    seasons: Vec<u32>,
    #[serde(default)]
    delete_torrents: bool,
    #[serde(default)]
    delete_library_files: bool,
}

pub(crate) async fn season_cleanup(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Path(id): Path<String>,
    Json(body): Json<SeasonCleanupInput>,
) -> Response {
    let (subscribe, _media) = match resolve_subscribe(&state, &id, user_id) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    if body.seasons.is_empty() {
        return ok(json!({ "queued": true, "binned": 0, "pending_cleared": 0 })).into_response();
    }
    let (rows, pending) = {
        let store = state.store.lock();
        if (body.delete_library_files || body.delete_torrents) && !user_is_admin(&store, user_id) {
            return err(
                StatusCode::FORBIDDEN,
                "subscription.cleanup_forbidden",
                "只有管理员可以联动清理下载任务或媒体库文件",
            );
        }
        let rows = body
            .delete_library_files
            .then(|| {
                store
                    .list_ledger()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|row| {
                        row.media_id == subscribe.media_id
                            && row
                                .season
                                .is_some_and(|season| body.seasons.contains(&season))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let pending = if body.delete_torrents {
            let mut pending = match store.load_pending(subscribe.id) {
                Ok(p) => p,
                Err(error) => {
                    tracing::error!(%error, subscribe_id = %subscribe.id, "读取在途下载任务失败");
                    return err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "store.error",
                        &error.to_string(),
                    );
                }
            };
            match store.load_pending_state(subscribe.id, "imported") {
                Ok(imported) => pending.extend(imported),
                Err(error) => {
                    tracing::error!(%error, subscribe_id = %subscribe.id, "读取已入库下载任务失败");
                    return err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "store.error",
                        &error.to_string(),
                    );
                }
            }
            pending
        } else {
            Vec::new()
        };
        (rows, pending)
    };
    let (binned, file_errors) = crate::http::file_delete::remove_rows(&state, rows).await;
    if !file_errors.is_empty() {
        return err(
            StatusCode::CONFLICT,
            "subscription.file_cleanup_failed",
            &file_errors.join("；"),
        );
    }

    let subscribe_id = subscribe.id;
    let selected: Vec<_> = pending
        .into_iter()
        .filter_map(|(_, pending)| {
            let season = pending
                .release_override
                .as_ref()
                .map(|release| release.season)
                .flatten()
                .or_else(|| release::parse(&pending.torrent.title).season);
            season
                .is_some_and(|season| body.seasons.contains(&season))
                .then(|| super::subscriptions::TorrentRemovalTarget {
                    subscribe_id,
                    torrent: pending.torrent,
                    downloader_id: pending.downloader_id,
                })
        })
        .collect();
    let (removed, pending_cleared, torrent_errors) =
        remove_torrents_from_client(&state, selected).await;
    if !torrent_errors.is_empty() {
        return err(
            StatusCode::BAD_GATEWAY,
            "subscription.downloader_cleanup_failed",
            &torrent_errors.join("；"),
        );
    }
    ok(json!({
        "queued": true,
        "binned": binned,
        "file_errors": [],
        "pending_cleared": pending_cleared,
        "removed_from_client": removed.len(),
    }))
    .into_response()
}
