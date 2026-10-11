use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{Coverage, Media, Subscribe, UserId};
use serde_json::{Value, json};

use crate::http::{err, ok_list};
use crate::management::ApiState;
use release;

pub(super) fn media_json(media: &Media, poster_url: Option<String>) -> Value {
    json!({
        "id": media.id.to_string(),
        "kind": media.kind.as_str(),
        "title": media.title,
        "year": media.year,
        "original_title": media.original_title,
        "tmdb_id": media.tmdb_id,
        "douban_id": media.douban_id,
        "tvdb_id": media.tvdb_id,
        "bangumi_id": media.bangumi_id,
        "anilist_id": media.anilist_id,
        "poster_url": poster_url,
    })
}

pub(crate) fn coverage_json(coverage: &Coverage) -> Value {
    match coverage {
        Coverage::Movie => json!({ "kind": "movie" }),
        Coverage::Tv {
            season,
            episode_from,
            episode_to,
        } => json!({
            "kind": "tv",
            "season": season,
            "episode_from": episode_from,
            "episode_to": episode_to,
        }),
    }
}

pub(crate) fn subscription_json(
    store: &crate::Store,
    _catalog: &dyn crate::catalog::Catalog,
    subscribe: &Subscribe,
    media: &Media,
    facts: &subscribe::SubscribeFacts,
) -> Value {
    let snapshot = super::snapshot::load_single_subscription_view(store, subscribe, media, facts);
    render_subscription_snapshot(&snapshot)
}

pub(crate) fn render_subscription_snapshot(
    snapshot: &super::snapshot::SubscriptionViewSnapshot,
) -> Value {
    let subscribe = &snapshot.subscribe;
    let media = &snapshot.media;
    let total = snapshot.total;
    let imported = snapshot.imported;
    let pending = &snapshot.pending;
    let facts = &snapshot.facts;

    let grabbing = count_pipeline_units(subscribe, pending, facts);
    let downloaded = (imported + grabbing).min(total);

    let created_at = if snapshot.created_at.is_empty() {
        "1970-01-01T00:00:00Z"
    } else {
        &snapshot.created_at
    };
    let updated_at = if snapshot.updated_at.is_empty() {
        "1970-01-01T00:00:00Z"
    } else {
        &snapshot.updated_at
    };

    let poster_choice = super::poster_view::determine_poster_from_candidate(
        media,
        snapshot.library_poster_candidate,
    );
    let poster_url = super::poster_view::poster_url(poster_choice);

    json!({
        "id": subscribe.id.to_string(),
        "media": media_json(media, poster_url),
        "user_id": subscribe.user_id.to_string(),
        "coverage": coverage_json(&subscribe.coverage),
        "fetch_mode": subscribe.fetch_mode.as_str(),
        "filter_id": subscribe.filter_id.to_string(),
        "wash_cut": subscribe.wash_cut,
        "keep_old_versions": subscribe.keep_old_versions,
        "wash_cut_filter_id": subscribe.wash_cut_filter_id.map(|id| id.to_string()),
        "full_season_pack": subscribe.full_season_pack,
        "downloader_id": subscribe.downloader_id.map(|id| id.to_string()),
        "library_id": subscribe.library_id.map(|id| id.to_string()),
        "target_library_status": snapshot.target_library_status,
        "target_library_error": snapshot.target_library_error,
        "tracking_state": subscribe.tracking_state,
        "follow_future": subscribe.follow_future,
        "search_interval_secs": subscribe.search_interval_secs,
        "progress": {
            "total": total,
            "imported": imported,
            "missing": (total - downloaded).max(0),
            "grabbing": grabbing,
            "downloaded": downloaded,
        },
        "created_at": created_at,
        "updated_at": updated_at,
    })
}

pub(crate) fn count_pipeline_units(
    subscribe: &Subscribe,
    pending: &[(i32, crate::store::PendingDownload)],
    facts: &subscribe::SubscribeFacts,
) -> i64 {
    if pending.is_empty() {
        return 0;
    }
    match subscribe.coverage {
        Coverage::Movie => {
            if facts.movie().is_none() {
                1
            } else {
                0
            }
        }
        Coverage::Tv {
            season,
            episode_from,
            episode_to,
        } => {
            let mut covered = std::collections::HashSet::new();
            for (_, p) in pending {
                let r = release::parse(&p.torrent.title);
                if r.season == Some(season) {
                    for (_, ep) in r.covered_episodes() {
                        let in_range =
                            ep >= episode_from && episode_to.map(|t| ep <= t).unwrap_or(true);
                        if in_range && facts.get(Some(season), Some(ep)).is_none() {
                            covered.insert(ep);
                        }
                    }
                }
            }
            // 若为全季包等未标明具体每集的种子，且有 pending，则兜底至少为 1（若仍有缺口）
            if covered.is_empty() {
                pending.len() as i64
            } else {
                covered.len() as i64
            }
        }
    }
}

pub(super) fn progress(
    store: &crate::Store,
    subscribe: &Subscribe,
    media: &Media,
    facts: &subscribe::SubscribeFacts,
) -> (i64, i64) {
    match subscribe.coverage {
        Coverage::Movie => (1, if facts.movie().is_some() { 1 } else { 0 }),
        Coverage::Tv {
            episode_to: Some(_),
            ..
        } => {
            let units = subscribe.coverage.units();
            let total = units.len() as i64;
            let imported = units
                .iter()
                .filter(|(s, ep)| facts.get(*s, *ep).is_some())
                .count() as i64;
            (total, imported)
        }
        Coverage::Tv {
            season,
            episode_from,
            episode_to: None,
        } => open_season_progress(store, media, facts, season, episode_from),
    }
}

fn open_season_progress(
    store: &crate::Store,
    media: &Media,
    facts: &subscribe::SubscribeFacts,
    season: u32,
    episode_from: u32,
) -> (i64, i64) {
    if let Some(count) = cached_season_episode_count(store, media, season).filter(|c| *c > 0) {
        let to = (count as u32)
            .max(episode_from)
            .min(episode_from.saturating_add(domain::Coverage::MAX_EPISODES.saturating_sub(1)));
        let total = i64::from(to.saturating_sub(episode_from)) + 1;
        let imported = (episode_from..=to)
            .filter(|ep| facts.get(Some(season), Some(*ep)).is_some())
            .count() as i64;
        return (total, imported);
    }
    let imported = facts
        .entries()
        .filter(|((s, e), _)| *s == Some(season) && e.is_some())
        .count() as i64;
    (imported.max(1), imported)
}

fn cached_season_episode_count(store: &crate::Store, media: &Media, season: u32) -> Option<i64> {
    let tmdb_id = media.tmdb_id.as_deref()?;
    let cached = store
        .get_catalog_cache("tmdb", &format!("/tv/{tmdb_id}?language=zh-CN"))
        .ok()
        .flatten()
        .or_else(|| {
            store
                .get_catalog_cache("tmdb", &format!("/tv/{tmdb_id}"))
                .ok()
                .flatten()
        })
        .or_else(|| cached_show_body(store, tmdb_id))?;
    let val: serde_json::Value = serde_json::from_str(&cached).ok()?;
    val.get("seasons")?
        .as_array()?
        .iter()
        .find(|s| s.get("season_number").and_then(Value::as_i64) == Some(i64::from(season)))?
        .get("episode_count")?
        .as_i64()
}

fn cached_show_body(store: &crate::Store, tmdb_id: &str) -> Option<String> {
    store
        .list_catalog_cache()
        .ok()?
        .into_iter()
        .find_map(|row| {
            (row.cache_key.contains(tmdb_id)
                && (row.cache_key.contains("language=zh-CN") || row.cache_key.ends_with(tmdb_id)))
            .then_some(row.body)
        })
}

pub(crate) async fn list_subscriptions(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<UserId>,
) -> Response {
    let state_for_read = state.clone();
    let snapshots = match tokio::task::spawn_blocking(move || {
        let store = state_for_read.store.lock();
        super::snapshot::load_visible_subscription_views(&store, user_id)
    })
    .await
    {
        Ok(Ok(views)) => views,
        Ok(Err(err_msg)) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &err_msg.to_string(),
            );
        }
        Err(join_err) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.join",
                &join_err.to_string(),
            );
        }
    };

    let mut rows = Vec::with_capacity(snapshots.len());
    for snapshot in snapshots {
        rows.push(render_subscription_snapshot(&snapshot));
    }
    ok_list(rows).into_response()
}
