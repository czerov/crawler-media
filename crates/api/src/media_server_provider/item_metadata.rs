use domain::{Media, UserId};
use media_server::provider::MediaItemMetadata;
use std::collections::BTreeMap;

use crate::Store;

mod artwork;
mod nfo;
mod people;

pub(super) use artwork::{backdrop_path, local_primary_image, primary_image_url};
use artwork::{primary_image_metadata, series_primary_image_tag};
use nfo::row_nfo_metadata;
pub(super) use people::{catalog_person_image_url, person_image_url, person_tmdb_id};
use people::{merge_people, people_from_nfo};

fn item_is_favorite(store: &Store, user_id: UserId, row: &domain::LedgerRow) -> bool {
    let season = row
        .season
        .map(|value| value as i32)
        .unwrap_or(crate::store::UNIT_WHOLE);
    let episode = row
        .episode
        .map(|value| value as i32)
        .unwrap_or(crate::store::UNIT_WHOLE);

    match store.unit_state(user_id, row.media_id, season, episode) {
        Ok(Some(state)) => state.favorite,
        Ok(None) => false,
        Err(error) => {
            tracing::warn!(%error, %user_id, media_id = %row.media_id, season = season, episode = episode, "failed to read Jellyfin favorite mark");
            false
        }
    }
}

pub(super) fn item_metadata(
    store: &Store,
    user_id: UserId,
    row: &domain::LedgerRow,
    media: &Media,
    is_series: bool,
    child_counts: Option<(usize, usize)>,
) -> MediaItemMetadata {
    let nfo = row_nfo_metadata(row, media, is_series);
    let (has_primary_image, primary_image_url) = primary_image_metadata(row, media, is_series);
    let primary_image_tag = series_primary_image_tag(row, media, is_series);
    let resume_state = row.season.zip(row.episode).and_then(|(season, episode)| {
        store
            .unit_state(user_id, row.media_id, season as i32, episode as i32)
            .ok()
            .flatten()
    });
    let (child_count, local_season_count) = child_counts
        .map(|(episodes, seasons)| (Some(episodes), Some(seasons)))
        .unwrap_or((None, None));
    let season_count = nfo
        .as_ref()
        .and_then(|metadata| metadata.number_of_seasons)
        .map(|count| count as usize)
        .or(local_season_count);
    let favorite_row = if is_series {
        domain::LedgerRow {
            season: None,
            episode: None,
            ..row.clone()
        }
    } else {
        row.clone()
    };
    let mut tracks = store
        .get_file_meta(&row.id.to_string())
        .ok()
        .flatten()
        .unwrap_or_default();
    match library::external_subtitle_tracks(std::path::Path::new(&row.path)) {
        Ok(sidecars) => {
            for sidecar in sidecars {
                let already_present = tracks
                    .subtitles
                    .iter()
                    .any(|subtitle| subtitle.is_external && subtitle.path == sidecar.path);
                if !already_present {
                    tracks.subtitles.push(sidecar);
                }
            }
        }
        Err(error) => tracing::warn!(
            %error,
            path = %row.path,
            "failed to discover Jellyfin external subtitles"
        ),
    }

    MediaItemMetadata {
        overview: nfo.as_ref().and_then(|metadata| metadata.plot.clone()),
        community_rating: nfo
            .as_ref()
            .and_then(|metadata| metadata.rating.as_deref())
            .and_then(|rating| rating.parse().ok()),
        vote_count: nfo.as_ref().and_then(|metadata| metadata.vote_count),
        genres: nfo
            .as_ref()
            .map(|metadata| metadata.genres.clone())
            .unwrap_or_default(),
        premiere_date: nfo.as_ref().and_then(|metadata| {
            if is_series {
                metadata.premiered.clone()
            } else {
                metadata
                    .aired
                    .clone()
                    .or_else(|| metadata.premiered.clone())
            }
        }),
        date_created: None,
        end_date: nfo.as_ref().and_then(|metadata| metadata.end_date.clone()),
        official_rating: nfo
            .as_ref()
            .and_then(|metadata| metadata.content_rating.clone()),
        status: nfo.as_ref().and_then(|metadata| metadata.status.clone()),
        original_language: nfo
            .as_ref()
            .and_then(|metadata| metadata.original_language.clone()),
        taglines: nfo
            .as_ref()
            .and_then(|metadata| metadata.tagline.clone())
            .into_iter()
            .collect(),
        studios: nfo
            .as_ref()
            .map(|metadata| metadata.studios.clone())
            .unwrap_or_default(),
        production_locations: nfo
            .as_ref()
            .map(|metadata| metadata.countries.clone())
            .unwrap_or_default(),
        number_of_episodes: nfo
            .as_ref()
            .and_then(|metadata| metadata.number_of_episodes),
        production_year: nfo
            .as_ref()
            .and_then(|metadata| metadata.year.as_deref())
            .and_then(|year| year.parse().ok())
            .or_else(|| media.year.map(i32::from)),
        original_title: nfo
            .as_ref()
            .and_then(|metadata| metadata.original_title.clone())
            .or_else(|| media.original_title.clone()),
        runtime_ticks: runtime_ticks(nfo.as_ref(), &tracks, is_series),
        provider_ids: provider_ids(media),
        episode_name: (!is_series)
            .then(|| nfo.as_ref().and_then(|metadata| metadata.title.clone()))
            .flatten(),
        people: people_from_nfo(nfo.as_ref()),
        is_favorite: item_is_favorite(store, user_id, &favorite_row),
        resume_updated_at: resume_state
            .map(|state| state.updated_at)
            .unwrap_or_default(),
        child_count,
        season_count,
        has_primary_image,
        has_backdrop_image: backdrop_path(row, is_series).is_some(),
        primary_image_tag,
        primary_image_url,
        tracks,
    }
}

pub(super) fn merge_catalog_metadata(
    mut item: MediaItemMetadata,
    media: &Media,
    catalog: &media::ItemMeta,
    episode: Option<&media::EpisodeMeta>,
) -> MediaItemMetadata {
    let episode_overview = episode.and_then(|metadata| metadata.overview.clone());
    if item.overview.as_deref().is_none_or(str::is_empty) {
        item.overview = episode_overview.or_else(|| catalog.overview.clone());
    }
    if item.community_rating.is_none() {
        item.community_rating = episode
            .and_then(|metadata| metadata.rating.as_deref())
            .or(catalog.rating.as_deref())
            .and_then(|rating| rating.parse().ok());
    }
    item.vote_count = item.vote_count.or_else(|| {
        episode
            .and_then(|metadata| metadata.vote_count)
            .or(catalog.vote_count)
    });
    item.genres = merge_names(item.genres, catalog.genres.iter().cloned());
    item.production_year = item.production_year.or_else(|| {
        catalog
            .release_date
            .as_deref()
            .and_then(|date| date.get(..4))
            .and_then(|year| year.parse().ok())
    });
    item.original_title = item.original_title.or_else(|| media.original_title.clone());
    if item.premiere_date.is_none() {
        item.premiere_date = episode
            .and_then(|metadata| metadata.air_date.clone())
            .or_else(|| catalog.release_date.clone());
    }
    item.end_date = item.end_date.or_else(|| catalog.last_air_date.clone());
    item.official_rating = item
        .official_rating
        .or_else(|| catalog.content_rating.clone());
    item.status = item.status.or_else(|| catalog.status.clone());
    item.original_language = item
        .original_language
        .or_else(|| catalog.original_language.clone());
    if item.taglines.is_empty() {
        item.taglines = catalog.tagline.clone().into_iter().collect();
    }
    item.studios = merge_names(item.studios, catalog.studios.iter().cloned());
    item.production_locations = merge_names(
        item.production_locations,
        catalog.origin_countries.iter().cloned(),
    );
    item.number_of_episodes = catalog.number_of_episodes.or(item.number_of_episodes);
    item.season_count = catalog
        .number_of_seasons
        .map(|count| count as usize)
        .or(item.season_count);
    if item.runtime_ticks.is_none() {
        item.runtime_ticks = episode
            .and_then(|metadata| metadata.runtime_minutes)
            .or_else(|| catalog.runtime_minutes.as_deref()?.parse::<u32>().ok())
            .and_then(minutes_to_ticks);
    }

    let tmdb_nfo = crate::scrape_metadata::nfo_from_tmdb(media, catalog);
    let catalog_people = people_from_nfo(Some(&tmdb_nfo));
    merge_people(&mut item.people, catalog_people);
    if let Some(episode) = episode {
        if item.episode_name.as_deref().is_none_or(str::is_empty) {
            item.episode_name = episode.name.clone();
        }
    }
    item
}

fn merge_names(mut current: Vec<String>, fallback: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = current
        .iter()
        .map(|name| name.to_lowercase())
        .collect::<std::collections::HashSet<_>>();
    for name in fallback {
        if seen.insert(name.to_lowercase()) {
            current.push(name);
        }
    }
    current
}

fn minutes_to_ticks(minutes: u32) -> Option<i64> {
    i64::from(minutes).checked_mul(60)?.checked_mul(10_000_000)
}

fn provider_ids(media: &Media) -> BTreeMap<String, String> {
    [
        ("Tmdb", media.tmdb_id.as_ref()),
        ("Tvdb", media.tvdb_id.as_ref()),
        ("Douban", media.douban_id.as_ref()),
        ("Bangumi", media.bangumi_id.as_ref()),
        ("AniList", media.anilist_id.as_ref()),
    ]
    .into_iter()
    .filter_map(|(name, value)| value.map(|value| (name.to_string(), value.clone())))
    .collect()
}

fn runtime_ticks(
    nfo: Option<&library::NfoMeta>,
    tracks: &library::Tracks,
    is_series: bool,
) -> Option<i64> {
    if is_series {
        return None;
    }
    nfo.and_then(|metadata| metadata.runtime_minutes.as_deref())
        .and_then(|minutes| minutes.parse::<i64>().ok())
        .and_then(|minutes| minutes.checked_mul(60)?.checked_mul(10_000_000))
        .or_else(|| {
            tracks
                .video
                .as_ref()
                .and_then(|video| video.duration_secs)
                .map(|seconds| (seconds * 10_000_000.0).round() as i64)
        })
}
