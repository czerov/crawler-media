use super::*;

pub(super) fn unit_played(
    store: &crate::Store,
    user_id: UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> bool {
    let units = if season >= 0 && episode >= 0 {
        [(season, episode), (season, crate::store::UNIT_WHOLE), (crate::store::UNIT_WHOLE, crate::store::UNIT_WHOLE)]
    } else if season >= 0 || episode >= 0 {
        [(season, episode), (crate::store::UNIT_WHOLE, crate::store::UNIT_WHOLE), (crate::store::UNIT_WHOLE, crate::store::UNIT_WHOLE)]
    } else {
        [(season, episode), (season, episode), (season, episode)]
    };
    let count = if season >= 0 && episode >= 0 { 3 } else if season >= 0 || episode >= 0 { 2 } else { 1 };
    let mut latest: Option<store::UnitState> = None;
    for (unit_season, unit_episode) in units.into_iter().take(count) {
        match store.unit_state(user_id, media_id, unit_season, unit_episode) {
            Ok(Some(state)) if latest.as_ref().is_none_or(|current| state.updated_at >= current.updated_at) => latest = Some(state),
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => tracing::warn!(%error, %user_id, %media_id, season = unit_season, episode = unit_episode, "failed to read Jellyfin played mark"),
        }
    }
    latest.is_some_and(|state| state.played)
}
pub(super) fn item_is_played(
    store: &crate::Store,
    user_id: UserId,
    row: &domain::LedgerRow,
) -> bool {
    unit_played(
        store,
        user_id,
        row.media_id,
        row.season
            .map(|value| value as i32)
            .unwrap_or(crate::store::UNIT_WHOLE),
        row.episode
            .map(|value| value as i32)
            .unwrap_or(crate::store::UNIT_WHOLE),
    )
}

pub(super) fn unit_play_count(
    store: &crate::Store,
    user_id: UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> i64 {
    match store.unit_state(user_id, media_id, season, episode) {
        Ok(Some(state)) => state.play_count,
        Ok(None) => 0,
        Err(error) => {
            tracing::warn!(%error, %user_id, %media_id, season, episode, "failed to read Jellyfin play count");
            0
        }
    }
}
pub(super) fn resolve_mark_unit(
    store: &crate::Store,
    user_id: UserId,
    id: &str,
) -> Option<(domain::MediaId, i32, i32)> {
    let season_target =
        crate::http::media_visibility::resolve_visible_season(store, id, Some(user_id));
    let (row, season) = match season_target {
        Some((row, season)) => (row, Some(season)),
        None => (
            crate::http::media_visibility::resolve_visible_row(store, id, Some(user_id))?,
            None,
        ),
    };
    let unit = if let Some(season) = season {
        (season as i32, crate::store::UNIT_WHOLE)
    } else {
        let compact_id = id.replace('-', "");
        let is_media_id =
            compact_id.eq_ignore_ascii_case(&row.media_id.to_string().replace('-', ""));
        if is_media_id || (row.season.is_none() && row.episode.is_none()) {
            (crate::store::UNIT_WHOLE, crate::store::UNIT_WHOLE)
        } else {
            (
                row.season
                    .map(|value| value as i32)
                    .unwrap_or(crate::store::UNIT_WHOLE),
                row.episode
                    .map(|value| value as i32)
                    .unwrap_or(crate::store::UNIT_WHOLE),
            )
        }
    };
    Some((row.media_id, unit.0, unit.1))
}

pub(super) fn write_favorite_mark(
    store: &crate::Store,
    user_id: UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
    favorite: bool,
    now: i64,
) -> Result<(), String> {
    let existing = store
        .unit_state(user_id, media_id, season, episode)
        .map_err(|error| {
            tracing::error!(%error, %user_id, %media_id, season, episode, "failed to read Jellyfin favorite target");
            error.to_string()
        })?;
    let played = existing
        .as_ref()
        .map(|state| state.played)
        .unwrap_or_else(|| unit_played(store, user_id, media_id, season, episode));
    store
        .upsert_unit(
            user_id,
            media_id,
            season,
            episode,
            existing.as_ref().map(|state| state.position_ms).unwrap_or(0),
            Some(played),
            Some(favorite),
            existing.as_ref().and_then(|state| state.duration_ms),
            existing.as_ref().and_then(|state| state.audio_track.as_deref()),
            existing
                .as_ref()
                .and_then(|state| state.subtitle_track.as_deref()),
            false,
            now,
        )
        .map_err(|error| {
            tracing::error!(%error, %user_id, %media_id, season, episode, "failed to update Jellyfin favorite mark");
            error.to_string()
        })
        .map(|_| ())
}

pub(super) fn persist_unplayed_override(
    store: &crate::Store,
    user_id: UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
    favorite: Option<bool>,
    now: i64,
) -> Result<(), String> {
    let current = store.unit_state(user_id, media_id, season, episode).map_err(|error| {
        tracing::error!(%error, %user_id, %media_id, season, episode, "failed to read Jellyfin mark after update");
        error.to_string()
    })?;
    if current.is_none() {
        store
            .upsert_unit(
                user_id, media_id, season, episode, 0, Some(false), favorite, None, None, None,
                false, now,
            )
            .map_err(|error| {
                tracing::error!(%error, %user_id, %media_id, season, episode, "failed to persist Jellyfin unplayed override");
                error.to_string()
            })?;
    }
    Ok(())
}
