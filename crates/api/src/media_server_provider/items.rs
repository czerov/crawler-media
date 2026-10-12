use super::user_marks::{item_is_played, unit_play_count, unit_played};
use super::*;
use std::time::SystemTime;

pub(super) fn resolve_fallback_item(
    state: &ApiState,
    user_id: UserId,
    id: &str,
    compact_id: &str,
) -> Option<MediaItemSnapshot> {
    let store = state.store.lock();
    let row = crate::http::media_visibility::resolve_visible_row(&store, id, Some(user_id))?;
    let row_id = row.id.to_string().replace('-', "");
    if row_id != compact_id && row.id.to_string() != id {
        return None;
    }
    let media = store.get_media(row.media_id).ok().flatten()?;
    drop(store);
    Some(fallback_snapshot(state, user_id, row, media, false))
}

pub(super) fn fallback_snapshot(
    state: &ApiState,
    user_id: UserId,
    row: domain::LedgerRow,
    media: domain::Media,
    is_series: bool,
) -> MediaItemSnapshot {
    // 锁内只取 owned DB 事实。
    let (marker, chapters, ticks, played, play_count, parent_id, facts) = {
        let store = state.store.lock();
        let marker = store
            .get_media_marker(row.media_id, row.season, row.episode)
            .ok()
            .flatten();
        let chapters = store
            .get_cached_chapters(&row.id.to_string())
            .ok()
            .flatten()
            .unwrap_or_default();
        let ticks = playback_ticks(&store, user_id, &row, &media);
        let is_episode =
            media.kind == MediaKind::Tv && row.season.is_some() && row.episode.is_some();
        let parent_id = if is_episode {
            media.id.to_string().replace('-', "")
        } else {
            crate::http::playback::library_id_for(&store, &media, &[&row])
                .unwrap_or_else(|| media.id.to_string().replace('-', ""))
        };
        let played = item_is_played(&store, user_id, &row);
        let play_count = unit_play_count(
            &store,
            user_id,
            row.media_id,
            row.season
                .map(|value| value as i32)
                .unwrap_or(crate::store::UNIT_WHOLE),
            row.episode
                .map(|value| value as i32)
                .unwrap_or(crate::store::UNIT_WHOLE),
        );
        let facts = item_metadata::item_db_facts(&store, user_id, &row, is_series);
        (marker, chapters, ticks, played, play_count, parent_id, facts)
    };
    // 锁外：NFO / stat / 外部字幕 IO。
    let metadata = with_date_created(
        item_metadata(
            facts,
            state.browse_files.as_ref(),
            &row,
            &media,
            is_series,
            None,
        ),
        file_date_created(state.browse_files.as_ref(), &row.path),
    );
    MediaItemSnapshot {
        played,
        play_count,
        unplayed_item_count: None,
        metadata,
        row,
        media,
        ticks,
        parent_id,
        is_series,
        chapters,
        intro_start_ms: marker.as_ref().and_then(|item| item.intro_start_ms),
        intro_end_ms: marker.as_ref().and_then(|item| item.intro_end_ms),
        outro_start_ms: marker.as_ref().and_then(|item| item.outro_start_ms),
        outro_end_ms: marker.as_ref().and_then(|item| item.outro_end_ms),
    }
}

pub(super) fn list_visible_items(
    provider: &ApiServerProvider,
    user_id: UserId,
    parent_id: Option<&str>,
) -> Result<Vec<MediaItemSnapshot>, String> {
    let store = provider.state.store.lock();
    let mut visible = Vec::new();
    let mut seen_series = std::collections::HashSet::new();
    let ledger_rows = store.list_ledger().map_err(|error| error.to_string())?;
    let mut show_episodes: HashMap<domain::MediaId, HashSet<(u32, u32)>> = HashMap::new();
    let mut show_seasons: HashMap<domain::MediaId, HashSet<u32>> = HashMap::new();
    let latest_ingest_by_media =
        latest_visible_ingest_by_media(&store, provider.state.browse_files.as_ref(), user_id, &ledger_rows);
    collect_visible_units(
        &store,
        user_id,
        &ledger_rows,
        &mut show_episodes,
        &mut show_seasons,
    );

    for row in ledger_rows {
        let Some(media) = store.get_media(row.media_id).ok().flatten() else {
            continue;
        };
        let Some(owner) = crate::http::playback::library_id_for(&store, &media, &[&row]) else {
            continue;
        };
        let series_id = media.id.to_string().replace('-', "");
        let requested_series = media.kind == MediaKind::Tv
            && parent_id.is_some_and(|id| id == series_id || id == media.id.to_string());

        if let Some(parent_id) = parent_id {
            if parent_id != owner && !requested_series {
                continue;
            }
        }

        if !crate::http::library::row_visible_to_user(&store, &row, &media, Some(user_id)) {
            continue;
        }

        let is_series = media.kind == MediaKind::Tv && !requested_series;
        if is_series && !seen_series.insert(media.id) {
            continue;
        }

        let date_created = if is_series {
            latest_ingest_by_media.get(&media.id).copied()
        } else {
            file_date_created(provider.state.browse_files.as_ref(), &row.path)
        };
        visible.push(visible_snapshot(
            &store,
            provider.state.browse_files.as_ref(),
            user_id,
            row,
            media,
            is_series,
            if requested_series { series_id } else { owner },
            &show_episodes,
            &show_seasons,
            date_created,
        ));
    }
    Ok(visible)
}

fn playback_ticks(
    store: &crate::Store,
    user_id: UserId,
    row: &domain::LedgerRow,
    media: &domain::Media,
) -> i64 {
    store
        .playback_unit_progress(user_id, row.media_id, row.season, row.episode)
        .ok()
        .flatten()
        .or_else(|| {
            (media.kind == MediaKind::Movie)
                .then(|| {
                    store
                        .playback_progress(user_id, row.media_id)
                        .ok()
                        .flatten()
                })
                .flatten()
        })
        .unwrap_or(0)
        * 10_000
}

fn visible_snapshot(
    store: &crate::Store,
    browse: &dyn crate::browse_file_read::BrowseFileRead,
    user_id: UserId,
    row: domain::LedgerRow,
    media: domain::Media,
    is_series: bool,
    parent_id: String,
    show_episodes: &HashMap<domain::MediaId, HashSet<(u32, u32)>>,
    show_seasons: &HashMap<domain::MediaId, HashSet<u32>>,
    date_created: Option<SystemTime>,
) -> MediaItemSnapshot {
    let ticks = playback_ticks(&store, user_id, &row, &media);

    let marker = store
        .get_media_marker(row.media_id, row.season, row.episode)
        .ok()
        .flatten();
    let chapters = store
        .get_cached_chapters(&row.id.to_string())
        .ok()
        .flatten()
        .unwrap_or_default();
    let (played, play_count, unplayed_item_count) =
        unit_counts(store, user_id, &row, &media, is_series, show_episodes);
    let child_counts = is_series.then(|| {
        (
            show_episodes.get(&media.id).map(HashSet::len).unwrap_or(0),
            show_seasons.get(&media.id).map(HashSet::len).unwrap_or(0),
        )
    });
    let metadata = with_date_created(
        item_metadata(
            item_metadata::item_db_facts(&store, user_id, &row, is_series),
            browse,
            &row,
            &media,
            is_series,
            child_counts,
        ),
        date_created,
    );

    MediaItemSnapshot {
        row,
        media,
        ticks,
        play_count,
        played,
        unplayed_item_count,
        parent_id,
        is_series,
        chapters,
        intro_start_ms: marker.as_ref().and_then(|m| m.intro_start_ms),
        intro_end_ms: marker.as_ref().and_then(|m| m.intro_end_ms),
        outro_start_ms: marker.as_ref().and_then(|m| m.outro_start_ms),
        outro_end_ms: marker.as_ref().and_then(|m| m.outro_end_ms),
        metadata,
    }
}

fn latest_visible_ingest_by_media(
    store: &crate::Store,
    browse: &dyn crate::browse_file_read::BrowseFileRead,
    user_id: UserId,
    rows: &[domain::LedgerRow],
) -> HashMap<domain::MediaId, SystemTime> {
    let mut latest = HashMap::new();
    for row in rows {
        let Some(media) = store.get_media(row.media_id).ok().flatten() else {
            continue;
        };
        if !crate::http::library::row_visible_to_user(store, row, &media, Some(user_id)) {
            continue;
        }
        let Some(created) = file_date_created(browse, &row.path) else {
            continue;
        };
        latest
            .entry(media.id)
            .and_modify(|current: &mut SystemTime| *current = (*current).max(created))
            .or_insert(created);
    }
    latest
}

fn file_date_created(
    browse: &dyn crate::browse_file_read::BrowseFileRead,
    path: &str,
) -> Option<SystemTime> {
    let metadata = match browse.metadata(std::path::Path::new(path)) {
        Ok(metadata) => metadata,
        Err(error) => {
            tracing::warn!(path, %error, "Jellyfin DateCreated unavailable for media file");
            return None;
        }
    };
    let created = metadata.created().or_else(|error| {
        tracing::warn!(path, %error, "Jellyfin DateCreated uses modified time because creation time is unavailable");
        metadata.modified()
    });
    match created {
        Ok(created) => Some(created),
        Err(error) => {
            tracing::warn!(path, %error, "Jellyfin DateCreated unavailable for media file");
            None
        }
    }
}

fn with_date_created(
    mut metadata: media_server::provider::MediaItemMetadata,
    date_created: Option<SystemTime>,
) -> media_server::provider::MediaItemMetadata {
    metadata.date_created = date_created.and_then(crate::compact_time::format_rfc3339_utc);
    metadata
}

fn unit_counts(
    store: &crate::Store,
    user_id: UserId,
    row: &domain::LedgerRow,
    media: &domain::Media,
    is_series: bool,
    show_episodes: &HashMap<domain::MediaId, HashSet<(u32, u32)>>,
) -> (bool, i64, Option<usize>) {
    if is_series {
        let units = show_episodes.get(&media.id);
        let total = units.map(HashSet::len).unwrap_or(0);
        let played = units
            .into_iter()
            .flat_map(|units| units.iter())
            .filter(|(season, episode)| {
                unit_played(&store, user_id, media.id, *season as i32, *episode as i32)
            })
            .count();
        let unplayed = total.saturating_sub(played);
        (total == 0 || unplayed == 0, 0, Some(unplayed))
    } else {
        let season = row
            .season
            .map(|value| value as i32)
            .unwrap_or(crate::store::UNIT_WHOLE);
        let episode = row
            .episode
            .map(|value| value as i32)
            .unwrap_or(crate::store::UNIT_WHOLE);
        (
            item_is_played(&store, user_id, &row),
            unit_play_count(&store, user_id, row.media_id, season, episode),
            None,
        )
    }
}

fn collect_visible_units(
    store: &crate::Store,
    user_id: UserId,
    ledger_rows: &[domain::LedgerRow],
    show_episodes: &mut HashMap<domain::MediaId, HashSet<(u32, u32)>>,
    show_seasons: &mut HashMap<domain::MediaId, HashSet<u32>>,
) {
    for row in ledger_rows {
        let Some(media) = store.get_media(row.media_id).ok().flatten() else {
            continue;
        };
        if media.kind != MediaKind::Tv
            || !crate::http::library::row_visible_to_user(&store, row, &media, Some(user_id))
        {
            continue;
        }
        if let (Some(season), Some(episode)) = (row.season, row.episode) {
            show_episodes
                .entry(media.id)
                .or_default()
                .insert((season, episode));
            show_seasons.entry(media.id).or_default().insert(season);
        }
    }
}
