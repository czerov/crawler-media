use std::collections::HashSet;

use crate::provider::MediaItemSnapshot;
use domain::MediaKind;

use super::ItemsQuery;

pub(super) fn filter_items(
    rows: Vec<MediaItemSnapshot>,
    query: &ItemsQuery,
) -> Vec<MediaItemSnapshot> {
    filter_items_except_played(rows, query)
        .into_iter()
        .filter(|item| matches_bool(item.played, query.is_played))
        .collect()
}

/// 除播放状态以外的全部过滤条件。`Items/Latest` 要自己决定播放状态那一档
/// （显式 `IsPlayed` 才筛，未指定时只把已看完的沉底，见 `library::played_last`），
/// 所以它先走这一个，再自己收口；其余路由用上面的 `filter_items` 即可。
pub(super) fn filter_items_except_played(
    rows: Vec<MediaItemSnapshot>,
    query: &ItemsQuery,
) -> Vec<MediaItemSnapshot> {
    let person_ids = query.person_ids.as_deref().map(parse_person_ids);
    rows.into_iter()
        .filter(|item| {
            matches_series(item, query.series_id.as_deref())
                && matches_season(item, query.season_id.as_deref())
                && matches_people(item, person_ids.as_ref())
                && matches_user_marks(item, query.filters.as_deref())
                && matches_bool(item.metadata.is_favorite, query.is_favorite)
                && matches_include_item_types(item, query.include_item_types.as_deref())
                && matches_is_folder(item, query.is_folder)
                && matches_search_term(item, query.search_term.as_deref())
                && matches_genres(item, query.genres.as_deref(), query.genre_ids.as_deref())
                && matches_studios(item, query.studio_ids.as_deref())
        })
        .collect()
}

fn matches_is_folder(item: &MediaItemSnapshot, is_folder: Option<bool>) -> bool {
    let Some(expected) = is_folder else {
        return true;
    };
    item.is_series == expected
}

fn matches_search_term(item: &MediaItemSnapshot, search_term: Option<&str>) -> bool {
    let Some(term) = search_term.map(str::trim).filter(|s| !s.is_empty()) else {
        return true;
    };
    let term_lower = term.to_lowercase();
    if item.media.title.to_lowercase().contains(&term_lower) {
        return true;
    }
    if let Some(orig) = &item.metadata.original_title {
        if orig.to_lowercase().contains(&term_lower) {
            return true;
        }
    }
    if let Some(overview) = &item.metadata.overview {
        if overview.to_lowercase().contains(&term_lower) {
            return true;
        }
    }
    false
}

fn matches_genres(item: &MediaItemSnapshot, genres: Option<&str>, genre_ids: Option<&str>) -> bool {
    let mut target_genres: Vec<&str> = Vec::new();
    if let Some(g) = genres {
        target_genres.extend(g.split(|c| c == ',' || c == '|').map(str::trim).filter(|s| !s.is_empty()));
    }
    if let Some(gid) = genre_ids {
        target_genres.extend(gid.split(|c| c == ',' || c == '|').map(str::trim).filter(|s| !s.is_empty()));
    }
    if target_genres.is_empty() {
        return true;
    }
    target_genres.iter().any(|tg| {
        item.metadata
            .genres
            .iter()
            .any(|g| {
                if g.eq_ignore_ascii_case(tg) {
                    return true;
                }
                let simple_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, g.as_bytes()).simple().to_string();
                let hyphen_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, g.as_bytes()).hyphenated().to_string();
                let prefixed = format!("genre_{simple_id}");
                tg.eq_ignore_ascii_case(&simple_id)
                    || tg.eq_ignore_ascii_case(&hyphen_id)
                    || tg.eq_ignore_ascii_case(&prefixed)
            })
    })
}

fn matches_studios(item: &MediaItemSnapshot, studio_ids: Option<&str>) -> bool {
    let Some(studios) = studio_ids.map(str::trim).filter(|s| !s.is_empty()) else {
        return true;
    };
    let targets: Vec<&str> = studios
        .split(|c| c == ',' || c == '|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if targets.is_empty() {
        return true;
    }
    targets.iter().any(|ts| {
        item.metadata
            .studios
            .iter()
            .any(|s| s.eq_ignore_ascii_case(ts))
    })
}

pub(super) fn sort_items(
    mut rows: Vec<MediaItemSnapshot>,
    sort_by: &str,
    sort_order: &str,
) -> Vec<MediaItemSnapshot> {
    let keys: Vec<_> = sort_by
        .split(',')
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .collect();
    let sort_orders: Vec<_> = sort_order.split(',').map(str::trim).collect();
    rows.sort_by(|left, right| {
        let ordering = keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                let ordering = match key.to_ascii_lowercase().as_str() {
                    "datecreated" => left.metadata.date_created.cmp(&right.metadata.date_created),
                    "sortname" | "name" => left
                        .media
                        .title
                        .to_lowercase()
                        .cmp(&right.media.title.to_lowercase()),
                    "parentindexnumber" => left
                        .row
                        .season
                        .cmp(&right.row.season),
                    "indexnumber" => left.row.episode.cmp(&right.row.episode),
                    _ => std::cmp::Ordering::Equal,
                };
                let order = sort_orders
                    .get(index)
                    .or_else(|| sort_orders.last())
                    .copied()
                    .unwrap_or("Descending");
                if order.eq_ignore_ascii_case("descending") {
                    ordering.reverse()
                } else {
                    ordering
                }
            })
            .find(|ordering| *ordering != std::cmp::Ordering::Equal)
            .unwrap_or_else(|| left.row.id.to_string().cmp(&right.row.id.to_string()));
        ordering
    });
    rows
}

fn matches_include_item_types(item: &MediaItemSnapshot, include_item_types: Option<&str>) -> bool {
    let Some(types) = include_item_types else {
        return true;
    };
    let actual = if item.is_series {
        "Series"
    } else {
        match item.media.kind {
            MediaKind::Movie => "Movie",
            MediaKind::Tv => "Episode",
            MediaKind::Video => "Video",
        }
    };
    types
        .split(',')
        .map(str::trim)
        .any(|kind| kind.eq_ignore_ascii_case(actual))
}

fn matches_bool(actual: bool, expected: Option<bool>) -> bool {
    expected.map_or(true, |expected| actual == expected)
}

fn matches_user_marks(item: &MediaItemSnapshot, filters: Option<&str>) -> bool {
    let Some(filters) = filters else {
        return true;
    };
    filters
        .split(',')
        .map(str::trim)
        .filter(|filter| !filter.is_empty())
        .all(|filter| match filter.to_ascii_lowercase().as_str() {
            "isfavorite" => item.metadata.is_favorite,
            "isplayed" => item.played,
            "isunplayed" => !item.played,
            _ => true,
        })
}

fn parse_person_ids(person_ids: &str) -> HashSet<String> {
    person_ids
        .split(',')
        .map(str::trim)
        .map(compact_id)
        .collect()
}

fn matches_series(item: &MediaItemSnapshot, series_id: Option<&str>) -> bool {
    let Some(series_id) = series_id else {
        return true;
    };
    !item.is_series
        && item.media.kind == MediaKind::Tv
        && compact_id(&item.media.id.to_string()) == compact_id(series_id)
}

fn matches_season(item: &MediaItemSnapshot, season_id: Option<&str>) -> bool {
    let Some(season_id) = season_id else {
        return true;
    };
    let series_id = compact_id(&item.media.id.to_string());
    super::parse_season_id(&series_id, season_id)
        .is_some_and(|season| !item.is_series && item.row.season == Some(season))
}

fn matches_people(item: &MediaItemSnapshot, person_ids: Option<&HashSet<String>>) -> bool {
    let Some(person_ids) = person_ids else {
        return true;
    };
    item.metadata.people.iter().any(|person| {
        person_ids.contains(&compact_id(&person.id))
            || person
                .tmdb_id
                .as_ref()
                .is_some_and(|tmdb_id| person_ids.contains(tmdb_id))
    })
}

fn compact_id(id: &str) -> String {
    id.replace('-', "").to_ascii_lowercase()
}
