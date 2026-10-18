//! G-H2: drop candidates whose season was only inferred (parser default S01)
//! when the catalog proves the episode number lies beyond S01 of the show.

use domain::{Coverage, Media, MediaKind, Subscribe, Torrent};

use crate::management::ApiState;

/// Filter `torrents` for `subscribe`. Must be called without the store lock.
pub(crate) fn drop_misnumbered(
    state: &ApiState,
    subscribe: &Subscribe,
    media: &Media,
    torrents: Vec<Torrent>,
) -> Vec<Torrent> {
    let (MediaKind::Tv, Some(tmdb_id), Coverage::Tv { season: 1, .. }) =
        (media.kind, media.tmdb_id.as_deref(), &subscribe.coverage)
    else {
        return torrents;
    };
    let inferred = |t: &Torrent| {
        let release = release::parse(&t.title);
        release
            .season_inferred
            .then(|| release.episode_span())
            .flatten()
    };
    if !torrents.iter().any(|t| inferred(t).is_some()) {
        return torrents;
    }
    let Ok(episodes) = state.catalog.season_episodes(tmdb_id, 1) else {
        return torrents;
    };
    let Some(last) = episodes.iter().map(|e| e.episode_number).max() else {
        return torrents;
    };
    torrents
        .into_iter()
        .filter(|t| match inferred(t) {
            Some((_, to)) if to > last => {
                tracing::info!(subscribe_id = %subscribe.id, torrent = %t.title, s01_episodes = last, "标题无季号且集号超出目录 S01 集数，跳过");
                false
            }
            _ => true,
        })
        .collect()
}

/// G-H2b: catalog season lengths, built only when some title has an inferred
/// season and an episode number beyond the catalog's S01. Call without the
/// store lock (catalog I/O).
pub(crate) fn absolute_map<'t>(
    state: &ApiState,
    media: &Media,
    titles: impl IntoIterator<Item = &'t str>,
) -> Option<subscribe::AbsoluteEpisodes> {
    const MAX_SEASONS: u32 = 50;
    let (MediaKind::Tv, Some(tmdb_id)) = (media.kind, media.tmdb_id.as_deref()) else {
        return None;
    };
    let highest = titles
        .into_iter()
        .filter_map(|title| {
            let release = release::parse(title);
            release
                .season_inferred
                .then(|| release.episode_span())
                .flatten()
        })
        .map(|(_, to)| to)
        .max()?;
    let count = |season: u32| {
        state
            .catalog
            .season_episodes(tmdb_id, season)
            .ok()
            .and_then(|eps| eps.iter().map(|e| e.episode_number).max())
    };
    let s01 = count(1)?;
    if highest <= s01 {
        return None;
    }
    let mut counts = vec![(1, s01)];
    let mut total = s01;
    for season in 2..=MAX_SEASONS {
        let Some(n) = count(season) else { break };
        counts.push((season, n));
        total += n;
        if total >= highest {
            break;
        }
    }
    Some(subscribe::AbsoluteEpisodes::new(counts))
}
