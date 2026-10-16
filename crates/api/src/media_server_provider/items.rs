use super::user_marks::{item_is_played, unit_play_count};
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
    let (marker, chapters, ticks, played, play_count, parent_id, facts, series_counts) = {
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
        let is_episode = media.kind == MediaKind::Tv && row.season.is_some() && row.episode.is_some();
        let parent_id = if is_episode {
            media.id.to_string().replace('-', "")
        } else {
            crate::http::playback::library_id_for(&store, &media, &[&row])
                .unwrap_or_else(|| media.id.to_string().replace('-', ""))
        };
        let (played, play_count, series_counts) = if is_series {
            let (played, unplayed, child_counts) = direct_series_rollup(&store, user_id, media.id);
            (played, 0, Some((unplayed, child_counts)))
        } else {
            let played = item_is_played(&store, user_id, &row);
            let play_count = unit_play_count(
                &store,
                user_id,
                row.media_id,
                row.season.map(|value| value as i32).unwrap_or(crate::store::UNIT_WHOLE),
                row.episode.map(|value| value as i32).unwrap_or(crate::store::UNIT_WHOLE),
            );
            (played, play_count, None)
        };
        let (unit, whole_unit) = item_metadata::row_units(&store, user_id, &row, is_series);
        let facts = item_metadata::item_db_facts(&store, &row, is_series, unit.as_ref(), whole_unit.as_ref());
        (marker, chapters, ticks, played, play_count, parent_id, facts, series_counts)
    };
    // 锁外：NFO / stat / 外部字幕 IO。
    let metadata = with_date_created(
        item_metadata(
            facts,
            state.browse_files.as_ref(),
            &row,
            &media,
            is_series,
            series_counts.map(|(_, counts)| counts),
        ),
        file_date_created(state.browse_files.as_ref(), &row.path),
    );
    MediaItemSnapshot {
        played,
        play_count,
        unplayed_item_count: series_counts.map(|(unplayed, _)| unplayed),
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

fn direct_series_rollup(
    store: &crate::Store,
    user_id: UserId,
    media_id: domain::MediaId,
) -> (bool, usize, (usize, usize)) {
    let Ok(snapshot) = super::read_snapshot::collect_server_read(store, user_id, None) else {
        return (false, 0, (0, 0));
    };
    let mut episodes = HashSet::new();
    let mut seasons = HashSet::new();
    for row in &snapshot.rows {
        if row.media_id != media_id { continue; }
        let Some(media) = snapshot.media.get(&media_id) else { continue };
        let Some(owner) = snapshot.ownership.owner(std::path::Path::new(&row.path), media.kind) else { continue };
        let accessible = snapshot.ownership.accepts_work(&owner.id)
            && if snapshot.is_admin { owner.admin_visible } else { owner.access_mode == "everyone" || owner.member_ids.contains(&user_id) };
        if accessible && media.kind == MediaKind::Tv {
            if let (Some(season), Some(episode)) = (row.season, row.episode) {
                episodes.insert((season, episode));
                seasons.insert(season);
            }
        }
    }
    let units = unit_index(&snapshot.units);
    let played_count = episodes.iter().filter(|(season, episode)| {
        effective_played(&units, media_id, *season as i32, *episode as i32)
    }).count();
    let total = episodes.len();
    (total == 0 || played_count == total, total.saturating_sub(played_count), (total, seasons.len()))
}

pub(super) async fn list_visible_items(
    provider: &ApiServerProvider,
    user_id: UserId,
    parent_id: Option<&str>,
) -> Result<Vec<MediaItemSnapshot>, String> {
    let state = provider.state.clone();
    let budget = state.http_work.clone();
    let parent_id = parent_id.map(str::to_owned);
    budget
        .run(std::time::Duration::from_secs(30), move || {
            list_visible_items_blocking(&state, user_id, parent_id.as_deref())
        })
        .await
        .map_err(|error| {
            tracing::error!(error_kind = %error, user_id = %user_id, "Jellyfin item listing blocking work failed");
            error.to_string()
        })?
}

fn list_visible_items_blocking(
    state: &ApiState,
    user_id: UserId,
    parent_id: Option<&str>,
) -> Result<Vec<MediaItemSnapshot>, String> {
    let store_guard = state.store.lock();
    let snapshot = collect_pending(&store_guard, user_id, parent_id)?;
    drop(store_guard);
    let (pending, ingest_paths) = snapshot;
    let browse = state.browse_files.as_ref();
    let latest_ingest = latest_ingest_by_media(browse, &ingest_paths);
    let mut visible = Vec::with_capacity(pending.len());
    for item in pending {
        let date_created = match &item.stat_path {
            Some(path) => file_date_created(browse, path),
            None => latest_ingest.get(&item.media.id).copied(),
        };
        visible.push(item.into_snapshot(browse, date_created));
    }
    Ok(visible)
}

type UnitIndex<'a> = HashMap<(domain::MediaId, i32, i32), &'a crate::store::PlaybackUnitSnapshot>;

/// 一次索引：`find_unit` 的线性扫描在剧集聚合里是 O(集数 × 单元数)。
fn unit_index(units: &[crate::store::PlaybackUnitSnapshot]) -> UnitIndex<'_> {
    units.iter().map(|unit| ((unit.media_id, unit.season, unit.episode), unit)).collect()
}

fn find_unit<'a>(units: &UnitIndex<'a>, media_id: domain::MediaId, season: i32, episode: i32) -> Option<&'a crate::store::PlaybackUnitSnapshot> {
    units.get(&(media_id, season, episode)).copied()
}

fn effective_played(
    units: &UnitIndex<'_>,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> bool {
    let episode_unit = find_unit(units, media_id, season, episode);
    if episode_unit.is_some_and(|unit| unit.state.played) {
        return true;
    }
    let episode_updated_at = episode_unit.map(|unit| unit.state.updated_at);
    if let Some(season_unit) = (season >= 0)
        .then(|| find_unit(units, media_id, season, crate::store::UNIT_WHOLE))
        .flatten()
        .filter(|unit| episode_updated_at.is_none_or(|updated_at| unit.state.updated_at > updated_at))
    {
        return season_unit.state.played;
    }
    find_unit(units, media_id, crate::store::UNIT_WHOLE, crate::store::UNIT_WHOLE)
        .filter(|unit| episode_updated_at.is_none_or(|updated_at| unit.state.updated_at > updated_at))
        .is_some_and(|unit| unit.state.played)
}
fn unit_counts_from_snapshot(
    units: &UnitIndex<'_>, row: &domain::LedgerRow, media: &domain::Media,
    is_series: bool, show_episodes: &HashMap<domain::MediaId, HashSet<(u32, u32)>>,
) -> (bool, i64, Option<usize>) {
    if is_series {
        let episodes = show_episodes.get(&media.id);
        let total = episodes.map(HashSet::len).unwrap_or(0);
        let played = episodes.into_iter().flat_map(|episodes| episodes.iter()).filter(|(season, episode)| {
            effective_played(units, media.id, *season as i32, *episode as i32)
        }).count();
        return (total == 0 || total == played, 0, Some(total.saturating_sub(played)));
    }
    let season = row.season.map(|value| value as i32).unwrap_or(crate::store::UNIT_WHOLE);
    let episode = row.episode.map(|value| value as i32).unwrap_or(crate::store::UNIT_WHOLE);
    let unit = find_unit(units, row.media_id, season, episode);
    let played = if media.kind == MediaKind::Tv && season >= 0 && episode >= 0 {
        effective_played(units, row.media_id, season, episode)
    } else {
        unit.is_some_and(|unit| unit.state.played)
    };
    (played, unit.map(|unit| unit.state.play_count).unwrap_or(0), None)
}

fn collect_pending(
    store: &crate::Store,
    user_id: UserId,
    parent_id: Option<&str>,
) -> Result<(Vec<PendingSnapshot>, Vec<(domain::MediaId, String)>), String> {
    let snapshot = super::read_snapshot::collect_server_read(store, user_id, parent_id)?;
    let ServerReadSnapshot { ownership, is_admin, media, rows: ledger_rows, units, .. } = snapshot;
    let requested_library = parent_id.and_then(|id| ownership.library(id)).map(|library| library.id.as_str());
    let mut show_episodes: HashMap<domain::MediaId, HashSet<(u32, u32)>> = HashMap::new();
    let mut show_seasons: HashMap<domain::MediaId, HashSet<u32>> = HashMap::new();
    let row_visible = |row: &domain::LedgerRow| -> bool {
        let Some(item) = media.get(&row.media_id) else { return false };
        let Some(owner) = ownership.owner(std::path::Path::new(&row.path), item.kind) else { return false };
        ownership.accepts_work(&owner.id)
            && requested_library.is_none_or(|library_id| owner.id == library_id)
            && if is_admin { owner.admin_visible } else { owner.access_mode == "everyone" || owner.member_ids.contains(&user_id) }
    };
    for row in &ledger_rows {
        let Some(item) = media.get(&row.media_id) else { continue };
        if item.kind != MediaKind::Tv || !row_visible(row) { continue; }
        if let (Some(season), Some(episode)) = (row.season, row.episode) {
            show_episodes.entry(item.id).or_default().insert((season, episode));
            show_seasons.entry(item.id).or_default().insert(season);
        }
    }
    let units = unit_index(&units);
    let ingest_paths = ledger_rows.iter().filter_map(|row| {
        let item = media.get(&row.media_id)?;
        row_visible(row).then(|| (item.id, row.path.clone()))
    }).collect::<Vec<_>>();
    let mut seen_series = HashSet::new();
    let mut pending = Vec::new();
    for row in ledger_rows {
        let Some(media) = media.get(&row.media_id).cloned() else { continue };
        let Some(owner) = ownership.owner(std::path::Path::new(&row.path), media.kind) else { continue };
        if !row_visible(&row) { continue; }
        let series_id = media.id.to_string().replace('-', "");
        let requested_series = media.kind == MediaKind::Tv && parent_id.is_some_and(|id| id == series_id || id == media.id.to_string());
        let is_series = media.kind == MediaKind::Tv && !requested_series;
        if is_series && !seen_series.insert(media.id) { continue; }
        let marker = store.get_media_marker(row.media_id, row.season, row.episode).map_err(|error| error.to_string())?;
        let chapters = store.get_cached_chapters(&row.id.to_string()).map_err(|error| error.to_string())?.unwrap_or_default();
        let (played, play_count, unplayed_item_count) = unit_counts_from_snapshot(&units, &row, &media, is_series, &show_episodes);
        let child_counts = is_series.then(|| (show_episodes.get(&media.id).map(HashSet::len).unwrap_or(0), show_seasons.get(&media.id).map(HashSet::len).unwrap_or(0)));
        let season = row.season.map(|value| value as i32).unwrap_or(crate::store::UNIT_WHOLE);
        let episode = row.episode.map(|value| value as i32).unwrap_or(crate::store::UNIT_WHOLE);
        let unit = find_unit(&units, row.media_id, season, episode);
        let whole_unit = is_series.then(|| find_unit(&units, row.media_id, crate::store::UNIT_WHOLE, crate::store::UNIT_WHOLE)).flatten();
        let facts = item_metadata::item_db_facts(store, &row, is_series, unit, whole_unit);
        let ticks = unit.map(|unit| unit.state.position_ms.saturating_mul(10_000)).unwrap_or(0);
        let stat_path = (!is_series).then(|| row.path.clone());
        pending.push(PendingSnapshot {
            parent_id: if requested_series { series_id } else { owner.id.clone() }, ticks,
            intro_start_ms: marker.as_ref().and_then(|item| item.intro_start_ms), intro_end_ms: marker.as_ref().and_then(|item| item.intro_end_ms),
            outro_start_ms: marker.as_ref().and_then(|item| item.outro_start_ms), outro_end_ms: marker.as_ref().and_then(|item| item.outro_end_ms),
            row, media, is_series, play_count, played, unplayed_item_count, chapters, facts, child_counts, stat_path,
        });
    }
    Ok((pending, ingest_paths))
}



/// 锁内收集的 owned DB 事实；文件 IO 全部留到锁外。
struct PendingSnapshot {
    row: domain::LedgerRow,
    media: domain::Media,
    is_series: bool,
    parent_id: String,
    ticks: i64,
    play_count: i64,
    played: bool,
    unplayed_item_count: Option<usize>,
    chapters: Vec<library::ChapterMarker>,
    intro_start_ms: Option<i64>,
    intro_end_ms: Option<i64>,
    outro_start_ms: Option<i64>,
    outro_end_ms: Option<i64>,
    facts: item_metadata::ItemDbFacts,
    child_counts: Option<(usize, usize)>,
    /// 非 series：DateCreated 需要 stat 这个路径；series：用 ingest 映射。
    stat_path: Option<String>,
}

impl PendingSnapshot {
    /// 锁外：NFO/stat/外部字幕 IO + 组装。
    fn into_snapshot(
        self,
        browse: &dyn crate::browse_file_read::BrowseFileRead,
        date_created: Option<SystemTime>,
    ) -> MediaItemSnapshot {
        let Self {
            row,
            media,
            is_series,
            parent_id,
            ticks,
            play_count,
            played,
            unplayed_item_count,
            chapters,
            intro_start_ms,
            intro_end_ms,
            outro_start_ms,
            outro_end_ms,
            facts,
            child_counts,
            stat_path: _,
        } = self;
        let metadata = with_date_created(
            item_metadata(facts, browse, &row, &media, is_series, child_counts),
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
            intro_start_ms,
            intro_end_ms,
            outro_start_ms,
            outro_end_ms,
            metadata,
        }
    }
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

/// 锁外：把「可见行的 ingest 时间」按 Media 取最大值。
fn latest_ingest_by_media(
    browse: &dyn crate::browse_file_read::BrowseFileRead,
    paths: &[(domain::MediaId, String)],
) -> HashMap<domain::MediaId, SystemTime> {
    let mut latest = HashMap::new();
    for (media_id, path) in paths {
        let Some(created) = file_date_created(browse, path) else {
            continue;
        };
        latest
            .entry(*media_id)
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


#[cfg(test)]
mod tests {
    use super::*;

    fn unit(
        media_id: domain::MediaId,
        season: i32,
        episode: i32,
        played: bool,
        updated_at: i64,
    ) -> crate::store::PlaybackUnitSnapshot {
        crate::store::PlaybackUnitSnapshot {
            media_id,
            season,
            episode,
            state: crate::store::UnitState {
                position_ms: 0,
                played,
                favorite: false,
                duration_ms: None,
                audio_track: None,
                subtitle_track: None,
                play_count: 0,
                updated_at,
            },
        }
    }

    fn fixture() -> (domain::Media, domain::LedgerRow) {
        let media = domain::Media {
            id: domain::MediaId::new(),
            kind: MediaKind::Tv,
            title: "Show".into(),
            year: None,
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        let row = domain::LedgerRow {
            id: domain::LedgerId::new(),
            media_id: media.id,
            path: "/tv/Show.S01E01.mkv".into(),
            season: Some(1),
            episode: Some(1),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Release,
            confidence: domain::Confidence::High,
            filter_score: None,
        };
        (media, row)
    }

    fn two_episodes(media_id: domain::MediaId) -> HashMap<domain::MediaId, HashSet<(u32, u32)>> {
        let mut episodes = HashMap::new();
        episodes.insert(media_id, HashSet::from([(1_u32, 1_u32), (1, 2)]));
        episodes
    }

    /// 整剧已看（只有 `(-1,-1)` 单元）时，没有自己单元的集也必须算已看。
    #[test]
    fn series_rollup_honours_the_whole_show_mark() {
        let (media, row) = fixture();
        let units = vec![unit(media.id, crate::store::UNIT_WHOLE, crate::store::UNIT_WHOLE, true, 10)];
        let index = unit_index(&units);
        let (played, _, unplayed) =
            unit_counts_from_snapshot(&index, &row, &media, true, &two_episodes(media.id));
        assert!(played, "whole-show mark must make every episode played");
        assert_eq!(unplayed, Some(0));
    }

    /// 更晚的单集标记优先于整剧标记（显式取消已看不被覆盖）。
    #[test]
    fn series_rollup_keeps_a_newer_episode_mark() {
        let (media, row) = fixture();
        let units = vec![
            unit(media.id, crate::store::UNIT_WHOLE, crate::store::UNIT_WHOLE, true, 10),
            unit(media.id, 1, 1, false, 20),
            unit(media.id, 1, 2, true, 5),
        ];
        let index = unit_index(&units);
        let (played, _, unplayed) =
            unit_counts_from_snapshot(&index, &row, &media, true, &two_episodes(media.id));
        assert!(!played, "a newer episode unmark must win over the whole-show mark");
        assert_eq!(unplayed, Some(1));
    }
}
