//! Jellyfin「先选择后重元数据」：轻候选 → 完整筛选/排序/计数 → 只为本页构建
//! tracks/chapters/NFO 细节。
//!
//! 与旧的全量路径（`items::list_visible_items`）的差别只在**工作量**，语义由
//! `media_server::provider::query` 的共享谓词保证一致：
//!
//! - 一次作用域读：库/剧/分集的作用域在 SQL 层收敛（`ledger_candidates_for_library`
//!   或索引化的 `ledger_for_media`），不再为每个 series 重读整库；
//! - series 折叠与分集展开都在同一份候选上完成，folder 归属先于折叠确定；
//! - 只有选中的那一页才做 NFO/stat/字幕发现与 tracks/chapters 组装；
//! - 任何读取失败都向上抛，绝不退化成「更小的一页」。

use std::sync::atomic::Ordering;
use std::time::Duration;

use domain::{MediaId, MediaKind, UserId};
use media_server::provider::query::{
    ParentScope, classify_genre_or_studio_facts, default_sort_for_items,
    item_matches_genre_or_studio, matches_query, sort_snapshots,
};
use media_server::provider::{MediaItemSnapshot, ServerItemPage, ServerItemQuery};

use super::enrichment::catalog_merge;
use super::item_metadata;
use super::items::with_date_created;
use super::*;

mod light;
mod scope;

use light::{
    build_light_items, merge_filter_facts, needs_filter_facts, paths_date_created,
    set_date_created, LightItem,
};
use scope::read_scope;

/// 选择阶段允许的 blocking 预算（与 `list_visible_items` 一致）。
const SELECTION_DEADLINE: Duration = Duration::from_secs(30);

pub(super) async fn query_visible_items(
    provider: &ApiServerProvider,
    user_id: UserId,
    query: ServerItemQuery,
) -> Result<ServerItemPage, String> {
    let state = provider.state.clone();
    let budget = state.http_work.clone();
    budget
        .run(SELECTION_DEADLINE, move || {
            select_items_blocking(&state, user_id, query)
        })
        .await
        .map_err(|error| {
            tracing::error!(error_kind = %error, user_id = %user_id, "Jellyfin 条目选择 blocking 工作失败");
            error.to_string()
        })?
}

pub(super) async fn next_up_items(
    provider: &ApiServerProvider,
    user_id: UserId,
    series_id: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<ServerItemPage, String> {
    let state = provider.state.clone();
    let budget = state.http_work.clone();
    let series_id = series_id.map(str::to_owned);
    budget
        .run(SELECTION_DEADLINE, move || {
            next_up_blocking(&state, user_id, series_id.as_deref(), offset, limit)
        })
        .await
        .map_err(|error| {
            tracing::error!(error_kind = %error, user_id = %user_id, "Jellyfin NextUp blocking 工作失败");
            error.to_string()
        })?
}

fn select_items_blocking(
    state: &ApiState,
    user_id: UserId,
    query: ServerItemQuery,
) -> Result<ServerItemPage, String> {
    let browse = state.browse_files.clone();
    let (scope, mut items) = {
        let store = state.store.lock();
        let scope = read_scope(&store, user_id, &query)?;
        let items = if scope.unresolved {
            Vec::new()
        } else {
            build_light_items(&scope, &query)
        };
        (scope, items)
    };
    if scope.unresolved {
        return Ok(ServerItemPage::default());
    }
    count_candidates(state, items.len());
    // 复杂 NFO 过滤的冷启动成本是线性的（每条候选一次 NFO 读）：显式记录，不假装
    // 全部落在 SQL seek 上。
    if needs_filter_facts(&query, scope.unknown_parent) {
        tracing::debug!(
            candidates = items.len(),
            "Jellyfin 选择按查询需要读取 NFO 公共字段（线性成本）"
        );
        for item in &mut items {
            let facts = item_metadata::filter_facts(
                &item.snapshot.row,
                &item.snapshot.media,
                item.snapshot.is_series,
            );
            item.snapshot.metadata =
                merge_filter_facts(std::mem::take(&mut item.snapshot.metadata), facts);
        }
    }
    if query.person_ids.is_some() {
        // 轻筛字段必须完整后选页：person 过滤不允许「选页后再 enrich 再筛」。
        for item in &mut items {
            if !item.snapshot.is_series {
                crate::http::library::ensure_probe_enqueued(state, &item.snapshot.row);
            }
            let merge = catalog_merge(state, &item.snapshot);
            if let Some(merge) = merge.as_ref() {
                let metadata = std::mem::take(&mut item.snapshot.metadata);
                item.snapshot.metadata = merge.apply(metadata, &item.snapshot.media);
            }
            item.catalog = merge;
        }
    }
    if scope.unknown_parent {
        let parent = query.parent_id.clone().unwrap_or_default();
        let classification = classify_genre_or_studio_facts(
            &parent,
            items.iter().map(|item| {
                (
                    item.snapshot.metadata.genres.as_slice(),
                    item.snapshot.metadata.studios.as_slice(),
                )
            }),
        );
        match classification {
            ParentScope::Genre(target) | ParentScope::Studio(target) => {
                items.retain(|item| {
                    item_matches_genre_or_studio(
                        &target,
                        item.snapshot.metadata.genres.as_slice(),
                        item.snapshot.metadata.studios.as_slice(),
                    )
                });
            }
            // 认不出 genre/studio 的 parent 一律空集合，绝不退化成整库。
            _ => return Ok(ServerItemPage::default()),
        }
    }
    let mut kept: Vec<LightItem> = items;
    kept.retain(|item| matches_query(&item.snapshot, &query));
    let (default_sort, default_order) = default_sort_for_items(kept.iter().map(|item| &item.snapshot));
    let sort_by = query.sort_by.clone().unwrap_or_else(|| default_sort.to_string());
    let sort_order = query
        .sort_order
        .clone()
        .unwrap_or_else(|| default_order.to_string());
    if sort_by
        .split(',')
        .any(|key| key.trim().eq_ignore_ascii_case("datecreated"))
    {
        for item in &mut kept {
            set_date_created(browse.as_ref(), item);
        }
    }
    sort_snapshots(
        &mut kept,
        |item| &item.snapshot,
        &sort_by,
        &sort_order,
    );
    let total = kept.len();
    let selected: Vec<LightItem> = kept.into_iter().skip(query.offset).take(query.limit).collect();
    let mut page_items = Vec::with_capacity(selected.len());
    for item in selected {
        page_items.push(heavy_snapshot(state, user_id, item)?);
    }
    count_heavy(state, page_items.len());
    Ok(ServerItemPage {
        items: page_items,
        total,
    })
}

fn next_up_blocking(
    state: &ApiState,
    user_id: UserId,
    series_id: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<ServerItemPage, String> {
    let query = next_up_query(series_id);
    let (scope, items) = {
        let store = state.store.lock();
        let scope = read_scope(&store, user_id, &query)?;
        let items = if scope.unresolved {
            Vec::new()
        } else {
            build_light_items(&scope, &query)
        };
        (scope, items)
    };
    if scope.unresolved {
        return Ok(ServerItemPage::default());
    }
    count_candidates(state, items.len());
    // 一次聚合：候选按 Media 分组（保持候选顺序），每个 Media 取 (季, 集) 最小的
    // 未看条目，再整体分页。显式给了 SeriesId 时按旧语义允许任意 Media（含电影）。
    let restrict_kind = series_id.is_none();
    let mut order: Vec<MediaId> = Vec::new();
    let mut groups: HashMap<MediaId, Vec<LightItem>> = HashMap::new();
    for item in items {
        if item.snapshot.is_series || (restrict_kind && item.snapshot.media.kind != MediaKind::Tv) {
            continue;
        }
        let media_id = item.snapshot.media.id;
        let group = groups.entry(media_id).or_default();
        if group.is_empty() {
            order.push(media_id);
        }
        group.push(item);
    }
    let mut next_up = Vec::new();
    for media_id in order {
        let mut group = groups.remove(&media_id).unwrap_or_default();
        group.sort_by_key(|item| {
            (
                item.snapshot.row.season.unwrap_or_default(),
                item.snapshot.row.episode.unwrap_or_default(),
            )
        });
        if let Some(episode) = group.into_iter().find(|item| !item.snapshot.played) {
            next_up.push(episode);
        }
    }
    let total = next_up.len();
    let selected: Vec<LightItem> = next_up.into_iter().skip(offset).take(limit).collect();
    let mut page_items = Vec::with_capacity(selected.len());
    for item in selected {
        page_items.push(heavy_snapshot(state, user_id, item)?);
    }
    count_heavy(state, page_items.len());
    Ok(ServerItemPage {
        items: page_items,
        total,
    })
}

/// NextUp 的候选读只需要「这个用户能看到的行」或「这个 Media 的行」。
fn next_up_query(series_id: Option<&str>) -> ServerItemQuery {
    ServerItemQuery {
        series_id: series_id.map(str::to_owned),
        // NextUp 要的是分集候选：显式只列分集，避免为「剧折叠」条目建快照。
        include_item_types: Some("Episode".into()),
        recursive: true,
        limit: usize::MAX,
        ..Default::default()
    }
}

/// 本页重元数据：tracks/chapters/intro-outro/NFO/stat/外部字幕都在这里。
fn heavy_snapshot(
    state: &ApiState,
    user_id: UserId,
    item: LightItem,
) -> Result<MediaItemSnapshot, String> {
    let browse = state.browse_files.as_ref();
    let LightItem {
        snapshot,
        child_counts,
        stat_paths,
        catalog,
        date_created,
    } = item;
    let MediaItemSnapshot {
        row,
        media,
        ticks,
        play_count,
        played,
        unplayed_item_count,
        parent_id,
        is_series,
        ..
    } = snapshot;
    let (facts, chapters, marker) = {
        let store = state.store.lock();
        let (unit, whole_unit) = item_metadata::row_units(&store, user_id, &row, is_series);
        let facts = item_metadata::item_db_facts(
            &store,
            &row,
            is_series,
            unit.as_ref(),
            whole_unit.as_ref(),
        );
        let chapters = store
            .get_cached_chapters(&row.id.to_string())
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        let marker = store
            .get_media_marker(row.media_id, row.season, row.episode)
            .map_err(|error| error.to_string())?;
        (facts, chapters, marker)
    };
    let mut metadata =
        item_metadata::item_metadata(facts, browse, &row, &media, is_series, child_counts);
    if let Some(catalog) = catalog.as_ref() {
        metadata = catalog.apply(metadata, &media);
    }
    let date_created =
        date_created.or_else(|| paths_date_created(browse, is_series, &stat_paths));
    Ok(MediaItemSnapshot {
        row,
        media,
        ticks,
        play_count,
        played,
        unplayed_item_count,
        parent_id,
        is_series,
        chapters,
        intro_start_ms: marker.as_ref().and_then(|item| item.intro_start_ms),
        intro_end_ms: marker.as_ref().and_then(|item| item.intro_end_ms),
        outro_start_ms: marker.as_ref().and_then(|item| item.outro_start_ms),
        outro_end_ms: marker.as_ref().and_then(|item| item.outro_end_ms),
        metadata: with_date_created(metadata, date_created),
    })
}

fn count_candidates(state: &ApiState, candidates: usize) {
    state
        .jellyfin_candidates
        .fetch_add(candidates, Ordering::SeqCst);
}

fn count_heavy(state: &ApiState, heavy: usize) {
    state
        .jellyfin_heavy_items
        .fetch_add(heavy, Ordering::SeqCst);
    tracing::debug!(
        heavy_items = heavy,
        candidates = state.jellyfin_candidates.load(Ordering::SeqCst),
        "Jellyfin 选择工作量：候选与重元数据条数"
    );
}
