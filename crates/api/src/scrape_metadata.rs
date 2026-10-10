//! Shared TMDB → NFO mapping for every metadata scrape entry point.

use crate::catalog::Catalog;
use crate::scrape_store::ScrapeStoreExt;
use domain::{LedgerRow, Media};
use std::path::{Path, PathBuf};

pub(crate) fn fetch_tmdb_metadata(
    state: &crate::management::ApiState,
    kind: domain::MediaKind,
    tmdb_id: &str,
) -> Result<Option<media::ItemMeta>, String> {
    let preferences = state
        .store
        .lock()
        .get_scrape_config()
        .ok()
        .map(|config| {
            (
                config.effective.language_priority,
                config.effective.cert_country_priority,
            )
        })
        .unwrap_or_else(|| {
            (
                crate::scrape_config::default_language_priority(),
                crate::scrape_config::default_cert_country_priority(),
            )
        });
    state
        .catalog
        .metadata_with_preferences(kind, tmdb_id, &preferences.0, &preferences.1)
}

pub(crate) fn preferred_language(state: &crate::management::ApiState) -> String {
    state
        .store
        .lock()
        .get_scrape_config()
        .ok()
        .map(|config| config.effective.primary_language().to_string())
        .unwrap_or_else(|| "zh-CN".into())
}

/// NFO sidecars to read for one ledger row, most specific first. TV keeps the
/// show-level `tvshow.nfo`; movies may carry either `<file>.nfo` or `movie.nfo`.
/// Shared so browse and wall never disagree about where metadata lives.
pub(crate) fn nfo_candidates(
    path: &Path,
    row: &LedgerRow,
    kind: domain::MediaKind,
) -> Vec<PathBuf> {
    match kind {
        domain::MediaKind::Tv => vec![show_root(path, row).join("tvshow.nfo")],
        _ => {
            let directory = path.parent().unwrap_or(path);
            vec![path.with_extension("nfo"), directory.join("movie.nfo")]
        }
    }
}

pub(crate) fn show_root(path: &Path, row: &LedgerRow) -> PathBuf {
    let directory = path.parent().unwrap_or(path);
    let dir_name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_ascii_lowercase())
        .unwrap_or_default();
    let is_season_directory = dir_name.starts_with("season")
        || (dir_name.contains(".s0") || dir_name.contains(".s1") || dir_name.contains(".s2"));
    if row.season.is_some() && is_season_directory {
        directory.parent().unwrap_or(directory).to_path_buf()
    } else {
        directory.to_path_buf()
    }
}

pub(crate) fn write_nfo(path: &Path, media: &Media, metadata: &library::NfoMeta) -> bool {
    match library::write_nfo(path, media, Some(metadata)) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "failed to write scraped NFO metadata");
            false
        }
    }
}

pub(crate) fn write_series_nfos(
    state: &crate::management::ApiState,
    media: &Media,
    tmdb_id: &str,
    show_root: &Path,
    rows: &[LedgerRow],
    show_metadata: &library::NfoMeta,
    language: &str,
) {
    let Some(sample) = rows.first() else { return; };
    let lease = match state.acquire_path_work(Path::new(&sample.path), media.kind) {
        Ok(lease) => lease, Err(_) => return,
    };
    let catalog = state.catalog.as_ref();
    write_nfo(&show_root.join("tvshow.nfo"), media, show_metadata);
    let seasons = rows
        .iter()
        .filter_map(|row| row.season)
        .collect::<std::collections::BTreeSet<_>>();
    for season in seasons {
        let episodes = match catalog.season_details_lang(tmdb_id, season, language) {
            Ok(episodes) if !episodes.is_empty() => episodes,
            Ok(_) => match catalog.season_details(tmdb_id, season) {
                Ok(episodes) => episodes,
                Err(error) => {
                    tracing::warn!(%error, tmdb_id, season, "failed to scrape TV season episode metadata");
                    continue;
                }
            },
            Err(error) => {
                tracing::warn!(%error, tmdb_id, season, "failed to scrape TV season episode metadata");
                continue;
            }
        };
        for row in rows.iter().filter(|row| row.season == Some(season)) {
            if lease.as_ref().is_some_and(|lease| lease.control().is_cancelled()) { return; }
            let Some(episode_number) = row.episode else {
                continue;
            };
            let Some(episode) = episodes
                .iter()
                .find(|episode| episode.episode_number == episode_number)
                .cloned()
            else {
                continue;
            };
            let path = Path::new(&row.path).with_extension("nfo");
            write_nfo(&path, media, &nfo_from_episode(row, episode));
        }
    }
}

pub(crate) fn nfo_from_tmdb(media: &Media, metadata: &media::ItemMeta) -> library::NfoMeta {
    library::NfoMeta {
        original_title: media.original_title.clone(),
        plot: metadata.overview.clone(),
        rating: metadata.rating.clone(),
        runtime_minutes: metadata.runtime_minutes.clone(),
        tagline: metadata.tagline.clone(),
        premiered: metadata.release_date.clone(),
        end_date: metadata.last_air_date.clone(),
        content_rating: metadata.content_rating.clone(),
        vote_count: metadata.vote_count,
        original_language: metadata.original_language.clone(),
        countries: metadata.origin_countries.clone(),
        studios: metadata.studios.clone(),
        status: metadata.status.clone(),
        number_of_seasons: metadata.number_of_seasons,
        number_of_episodes: metadata.number_of_episodes,
        directors: metadata.directors.clone(),
        creators: metadata.creators.clone(),
        genres: metadata.genres.clone(),
        cast: metadata
            .cast
            .iter()
            .map(|member| library::CastMember {
                name: member.name.clone(),
                role: member.role.clone(),
                tmdb_id: member.person_id.map(|id| id.to_string()),
                thumb: member.avatar_path.clone(),
                order: Some(member.order),
            })
            .collect(),
        ..Default::default()
    }
}

pub(crate) fn nfo_from_episode(row: &LedgerRow, episode: media::EpisodeMeta) -> library::NfoMeta {
    library::NfoMeta {
        title: episode.name,
        plot: episode.overview,
        runtime_minutes: episode.runtime_minutes.map(|value| value.to_string()),
        rating: episode.rating,
        vote_count: episode.vote_count,
        aired: episode.air_date,
        season: row.season.map(|value| value as u32),
        episode: row.episode,
        thumb: episode
            .still_path
            .map(|path| format!("https://image.tmdb.org/t/p/w300{path}")),
        ..Default::default()
    }
}
