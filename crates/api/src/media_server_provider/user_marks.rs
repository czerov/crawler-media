use super::*;

pub(super) fn unit_played(
    store: &crate::Store,
    user_id: UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> bool {
    let mut units: [Option<crate::store::UnitState>; 3] = [None, None, None];
    let keys = [
        (season, episode),
        (season, crate::store::UNIT_WHOLE),
        (crate::store::UNIT_WHOLE, crate::store::UNIT_WHOLE),
    ];
    for (index, (unit_season, unit_episode)) in keys.into_iter().enumerate() {
        if index > 0 && (season < 0 || episode < 0) {
            continue;
        }
        match store.unit_state(user_id, media_id, unit_season, unit_episode) {
            Ok(Some(state)) => units[index] = Some(state),
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, %user_id, %media_id, season = unit_season, episode = unit_episode, "failed to read Jellyfin played mark"),
        }
    }
    resolve_effective_played(
        units[0].as_ref().map(|unit| (unit, unit.updated_at)),
        units[1].as_ref().map(|unit| (unit, unit.updated_at)),
        units[2].as_ref().map(|unit| (unit, unit.updated_at)),
    )
}

pub(super) fn resolve_effective_played(
    episode_unit: Option<(&crate::store::UnitState, i64)>,
    season_unit: Option<(&crate::store::UnitState, i64)>,
    show_unit: Option<(&crate::store::UnitState, i64)>,
) -> bool {
    if episode_unit.is_some_and(|(unit, _)| unit.played) {
        return true;
    }
    let episode_updated_at = episode_unit.map(|(_, updated_at)| updated_at);
    if let Some((_, updated_at)) = [season_unit, show_unit]
        .into_iter()
        .flatten()
        .filter(|(unit, _)| unit.played)
        .max_by_key(|(_, updated_at)| *updated_at)
        && episode_updated_at.is_none_or(|episode_updated_at| updated_at > episode_updated_at)
    {
        return true;
    }
    if episode_unit.is_some() {
        return false;
    }
    [season_unit, show_unit]
        .into_iter()
        .flatten()
        .max_by_key(|(_, updated_at)| *updated_at)
        .is_some_and(|(unit, _)| unit.played)
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
    _now: i64,
) -> Result<(), String> {
    let played = store
        .unit_state(user_id, media_id, season, episode)
        .map_err(|error| error.to_string())?
        .map(|state| state.played)
        .unwrap_or_else(|| unit_played(store, user_id, media_id, season, episode));
    store
        .set_unit_favorite(user_id, media_id, season, episode, favorite, played)
        .map_err(|error| {
            tracing::error!(%error, %user_id, %media_id, season, episode, "failed to update Jellyfin favorite mark");
            error.to_string()
        })
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
