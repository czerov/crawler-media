//! 轻候选：只有过滤/排序/计数需要的字段。
//!
//! 排序/过滤需要的 NFO 公共字段（genre/studio/person/search）只在查询真的用到时
//! 才读（线性成本，调用方显式记录）；tracks/chapters/NFO 细节留给选中的那一页。

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::SystemTime;

use domain::{LedgerRow, Media, MediaId, MediaKind};
use media_server::provider::{MediaItemMetadata, MediaItemSnapshot, ServerItemQuery};

use super::super::items::{
    UnitIndex, file_date_created, find_unit, unit_counts_from_snapshot, unit_index,
};
use super::scope::{in_scope_folder, row_visible, ScopedRead};

pub(super) struct LightItem {
    pub(super) snapshot: MediaItemSnapshot,
    /// series 折叠条目：作用域内可见的分集/季数。
    pub(super) child_counts: Option<(usize, usize)>,
    /// 非 series：DateCreated 用这条路径 stat；series：这些行取 ingest 最大值。
    pub(super) stat_paths: Vec<String>,
    /// person 过滤用到的 catalog 载荷：本页重建 metadata 时按同一份载荷重放。
    pub(super) catalog: Option<super::enrichment::CatalogMerge>,
    /// 已为排序算出的 DateCreated，避免本页再 stat 一次。
    pub(super) date_created: Option<SystemTime>,
}

/// 折叠/展开都在同一份候选上完成：folder 归属先定，再决定剧条目与分集条目。
pub(super) fn build_light_items(scope: &ScopedRead, query: &ServerItemQuery) -> Vec<LightItem> {
    let units = unit_index(&scope.units);
    let mut show_episodes: HashMap<MediaId, HashSet<(u32, u32)>> = HashMap::new();
    let mut show_seasons: HashMap<MediaId, HashSet<u32>> = HashMap::new();
    for row in &scope.rows {
        let Some(media) = scope.media.get(&row.media_id) else {
            continue;
        };
        if media.kind != MediaKind::Tv || !row_visible(scope, row) || !in_scope_folder(scope, row) {
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
    let wants_episodes = query.wants_episodes();
    let wants_series = query.wants_series();
    let mut order: Vec<MediaId> = Vec::new();
    let mut groups: HashMap<MediaId, Vec<&LedgerRow>> = HashMap::new();
    for row in &scope.rows {
        if !row_visible(scope, row) || !in_scope_folder(scope, row) {
            continue;
        }
        let group = groups.entry(row.media_id).or_default();
        if group.is_empty() {
            order.push(row.media_id);
        }
        group.push(row);
    }
    let mut items = Vec::new();
    for media_id in order {
        let Some(media) = scope.media.get(&media_id).cloned() else {
            continue;
        };
        let rows = groups.remove(&media_id).unwrap_or_default();
        let Some(first) = rows.first().copied() else {
            continue;
        };
        let series_parent_id = media.id.to_string().replace('-', "");
        let library_id = scope
            .ownership
            .owner(Path::new(&first.path), media.kind)
            .map(|owner| owner.id.clone())
            .unwrap_or_else(|| series_parent_id.clone());
        let all_paths: Vec<String> = rows.iter().map(|row| row.path.clone()).collect();
        if scope.media_scoped {
            // 读已经收敛到单个 Media：一行一条，不折叠（旧 `requested_series` 语义）。
            for row in rows {
                items.push(light_item(
                    &units,
                    &show_episodes,
                    row,
                    &media,
                    false,
                    None,
                    series_parent_id.clone(),
                ));
            }
            continue;
        }
        if media.kind != MediaKind::Tv {
            for row in rows {
                items.push(light_item(
                    &units,
                    &show_episodes,
                    row,
                    &media,
                    false,
                    None,
                    library_id.clone(),
                ));
            }
            continue;
        }
        if wants_episodes {
            for row in rows {
                items.push(light_item(
                    &units,
                    &show_episodes,
                    row,
                    &media,
                    false,
                    None,
                    series_parent_id.clone(),
                ));
            }
        }
        if wants_series {
            let counts = (
                show_episodes.get(&media_id).map(HashSet::len).unwrap_or(0),
                show_seasons.get(&media_id).map(HashSet::len).unwrap_or(0),
            );
            items.push(light_item(
                &units,
                &show_episodes,
                first,
                &media,
                true,
                Some(counts),
                library_id,
            ));
            if let Some(item) = items.last_mut() {
                // series 的 DateCreated 取该 Media 在作用域内可见行的 ingest 最大值。
                item.stat_paths = all_paths;
            }
        }
    }
    items
}

pub(super) fn light_item(
    units: &UnitIndex<'_>,
    show_episodes: &HashMap<MediaId, HashSet<(u32, u32)>>,
    row: &LedgerRow,
    media: &Media,
    is_series: bool,
    child_counts: Option<(usize, usize)>,
    parent_id: String,
) -> LightItem {
    let season = row
        .season
        .map(|value| value as i32)
        .unwrap_or(crate::store::UNIT_WHOLE);
    let episode = row
        .episode
        .map(|value| value as i32)
        .unwrap_or(crate::store::UNIT_WHOLE);
    let unit = find_unit(units, row.media_id, season, episode);
    let (played, play_count, unplayed_item_count) =
        unit_counts_from_snapshot(units, row, media, is_series, show_episodes);
    let favorite_unit = if is_series {
        find_unit(
            units,
            row.media_id,
            crate::store::UNIT_WHOLE,
            crate::store::UNIT_WHOLE,
        )
    } else {
        unit
    };
    LightItem {
        snapshot: MediaItemSnapshot {
            row: row.clone(),
            media: media.clone(),
            ticks: if is_series {
                0
            } else {
                unit.map(|unit| unit.state.position_ms.saturating_mul(10_000))
                    .unwrap_or(0)
            },
            play_count,
            played,
            unplayed_item_count,
            parent_id,
            is_series,
            chapters: Vec::new(),
            intro_start_ms: None,
            intro_end_ms: None,
            outro_start_ms: None,
            outro_end_ms: None,
            metadata: MediaItemMetadata {
                is_favorite: favorite_unit.is_some_and(|unit| unit.state.favorite),
                ..Default::default()
            },
        },
        child_counts,
        stat_paths: vec![row.path.clone()],
        catalog: None,
        date_created: None,
    }
}

pub(super) fn needs_filter_facts(query: &ServerItemQuery, unknown_parent: bool) -> bool {
    unknown_parent
        || query
            .search_term
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || query
            .genres
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || query
            .genre_ids
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || query
            .studio_ids
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || query
            .person_ids
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

pub(super) fn merge_filter_facts(
    mut metadata: MediaItemMetadata,
    facts: MediaItemMetadata,
) -> MediaItemMetadata {
    metadata.overview = facts.overview;
    metadata.genres = facts.genres;
    metadata.studios = facts.studios;
    metadata.original_title = facts.original_title;
    metadata.people = facts.people;
    metadata
}

pub(super) fn set_date_created(browse: &dyn crate::browse_file_read::BrowseFileRead, item: &mut LightItem) {
    let created = item_date_created(browse, item);
    item.date_created = created;
    item.snapshot.metadata.date_created = created.and_then(crate::compact_time::format_rfc3339_utc);
}

pub(super) fn item_date_created(
    browse: &dyn crate::browse_file_read::BrowseFileRead,
    item: &LightItem,
) -> Option<SystemTime> {
    paths_date_created(browse, item.snapshot.is_series, &item.stat_paths)
}

/// 非 series 取该行的 stat 时间；series 取作用域内可见行的 ingest 最大值
/// （与旧 `latest_ingest_by_media` 同口径）。
pub(super) fn paths_date_created(
    browse: &dyn crate::browse_file_read::BrowseFileRead,
    is_series: bool,
    paths: &[String],
) -> Option<SystemTime> {
    if is_series {
        paths
            .iter()
            .filter_map(|path| file_date_created(browse, path))
            .max()
    } else {
        paths
            .first()
            .and_then(|path| file_date_created(browse, path))
    }
}
