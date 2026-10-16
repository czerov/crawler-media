use std::collections::HashMap;
use std::path::Path;

use domain::{LedgerRow, Media, UserId};
use serde_json::{Value, json};

use crate::http::library::artwork_url;
use crate::store::Store;

#[derive(Debug, Clone)]
pub struct EpisodeRow {
    pub season: u32,
    pub episode: u32,
    pub file_ids: Vec<String>,
    pub file_name: String,
    pub local_still: Option<String>,
    pub tmdb_still: Option<String>,
    pub backdrop: Option<String>,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub position_ms: i64,
    pub played: bool,
    pub progress_percent: Option<i64>,
}

pub fn format_episodes_json(episodes: &[EpisodeRow], media_item_id: &str) -> Vec<Value> {
    episodes
        .iter()
        .map(|ep| {
            let still_url = ep
                .local_still
                .clone()
                .or_else(|| ep.tmdb_still.clone())
                .or_else(|| ep.backdrop.clone());
            let name = ep.name.clone().unwrap_or_else(|| ep.file_name.clone());
            json!({
                "episode_number": ep.episode,
                "name": name,
                "overview": ep.overview,
                "air_date": null,
                "still_url": still_url,
                "owned": true,
                "file_ids": ep.file_ids,
                "position_ms": ep.position_ms,
                "played": ep.played,
                "watched": ep.played,
                "progress_percent": ep.progress_percent,
                "media_item_id": media_item_id,
                "season_number": ep.season,
            })
        })
        .collect()
}

pub fn aggregate_visible_episodes(
    store: &Store,
    media: &Media,
    rows: &[LedgerRow],
    target_season: Option<u32>,
    user_id: Option<UserId>,
    season_metas: &HashMap<u32, HashMap<u32, media::EpisodeMeta>>,
) -> Vec<EpisodeRow> {
    aggregate_visible_episodes_with(
        store,
        media,
        rows,
        target_season,
        user_id,
        season_metas,
        &crate::browse_file_read::LocalBrowseFileRead,
    )
}

pub fn aggregate_visible_episodes_with(
    store: &Store,
    media: &Media,
    rows: &[LedgerRow],
    target_season: Option<u32>,
    user_id: Option<UserId>,
    season_metas: &HashMap<u32, HashMap<u32, media::EpisodeMeta>>,
    files: &dyn crate::browse_file_read::BrowseFileRead,
) -> Vec<EpisodeRow> {
    let mut unit_states = HashMap::new();
    if let Some(uid) = user_id {
        for row in rows {
            let Some(e) = row.episode else { continue };
            let s = row.season.unwrap_or(1);
            if let Ok(st) = store.unit_state(uid, media.id, s as i32, e as i32) {
                unit_states.insert((s, e), st);
            }
        }
    }
    aggregate_visible_episodes_owned(
        rows,
        target_season,
        &unit_states,
        season_metas,
        files,
    )
}

/// Bounded out-of-lock aggregation: takes owned rows, pre-loaded unit states and file seam.
pub fn aggregate_visible_episodes_owned(
    rows: &[LedgerRow],
    target_season: Option<u32>,
    unit_states: &HashMap<(u32, u32), Option<crate::store::UnitState>>,
    season_metas: &HashMap<u32, HashMap<u32, media::EpisodeMeta>>,
    files: &dyn crate::browse_file_read::BrowseFileRead,
) -> Vec<EpisodeRow> {
    let mut grouped: HashMap<(u32, u32), (Vec<String>, String, Option<String>, Option<String>)> =
        HashMap::new();

    for row in rows {
        let Some(ep_num) = row.episode else {
            continue;
        };
        let season_num = row.season.unwrap_or(1);
        if let Some(target) = target_season {
            if season_num != target {
                continue;
            }
        }

        let file_path = Path::new(&row.path);
        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();

        let local_still =
            crate::episode_still::existing_with(file_path, files).map(|_| artwork_url("stills", row.id));
        // 只有磁盘上真有 fanart 才给地址。没有剧照时卡片用集号占位，
        // 不再对每一集打出必定 404 的 /fanart/{id}。
        let backdrop = if row.season.is_some()
            && crate::http::library::backdrop_path_with(row, files).is_some()
        {
            Some(artwork_url("fanart", row.id))
        } else {
            None
        };

        let entry = grouped
            .entry((season_num, ep_num))
            .or_insert_with(|| (Vec::new(), file_name, local_still, backdrop));
        entry.0.push(row.id.to_string());
    }

    let mut keys: Vec<(u32, u32)> = grouped.keys().copied().collect();
    keys.sort();

    keys.into_iter()
        .map(|(season, episode)| {
            let (file_ids, file_name, local_still, backdrop) = grouped.remove(&(season, episode)).unwrap();
            let meta = season_metas
                .get(&season)
                .and_then(|ep_map| ep_map.get(&episode));

            let (name, overview, tmdb_still) = match meta {
                Some(m) => (
                    m.name.clone(),
                    m.overview.clone(),
                    m.still_path.as_ref().map(|p| format!("https://image.tmdb.org/t/p/w300{p}")),
                ),
                None => (None, None, None),
            };

            let (position_ms, played, progress_percent) = match unit_states.get(&(season, episode)).and_then(|state| state.as_ref()) {
                Some(ps) => {
                    let percent = if !ps.played {
                        ps.duration_ms
                            .and_then(|d| if d > 0 { Some(((ps.position_ms * 100) / d).clamp(0, 100)) } else { None })
                    } else {
                        None
                    };
                    (ps.position_ms, ps.played, percent)
                }
                None => (0, false, None),
            };

            EpisodeRow {
                season,
                episode,
                file_ids,
                file_name,
                local_still,
                tmdb_still,
                backdrop,
                name,
                overview,
                position_ms,
                played,
                progress_percent,
            }
        })
        .collect()
}
