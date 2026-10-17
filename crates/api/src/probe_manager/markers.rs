use std::collections::HashSet;
use std::time::Instant;

use domain::LedgerRow;
use marker::FINGERPRINT_ALGORITHM_VERSION;

use super::{ProbeManager, ProbeUnit};
use crate::Store;
use crate::fingerprint_job::{fingerprint_cache_key, media_source_version};
use crate::scrape_store::ScrapeStoreExt;
use crate::store::{MarkerResultReplacement, StoredMediaMarker};

struct SeasonFingerprintSamples {
    all_rows: Vec<LedgerRow>,
    season_rows: Vec<LedgerRow>,
    intros: Vec<(LedgerRow, Vec<u32>)>,
    outros: Vec<(LedgerRow, Vec<u32>, i64)>,
}

struct MarkerComparison {
    samples: SeasonFingerprintSamples,
    markers: Vec<StoredMediaMarker>,
    intro_sample_episodes: HashSet<u32>,
    outro_sample_episodes: HashSet<u32>,
    intro_count: usize,
    outro_count: usize,
    started_at: Instant,
}

pub(super) fn compare_and_store_markers(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
) -> Result<(), crate::store::StoreError> {
    if !unit.overwrite_markers && season_input_unchanged(mgr, unit)? {
        tracing::debug!(
            media_id = %unit.row.media_id,
            season = unit.row.season.unwrap_or(1),
            "【片头片尾】声纹输入未变化，跳过整季比对"
        );
        return Ok(());
    }
    let comparison = compare_season_fingerprints(mgr, unit)?;
    let _ = remember_season_digest(mgr, unit);
    let season = unit.row.season.unwrap_or(1);
    if comparison.markers.is_empty() && !unit.overwrite_markers {
        tracing::warn!(
            media_id = %unit.row.media_id,
            season,
            intro_samples = comparison.intro_count,
            outro_samples = comparison.outro_count,
            elapsed_ms = comparison.started_at.elapsed().as_millis() as u64,
            "【片头片尾】当前声纹没有形成可用匹配，保留现有标记"
        );
        return Ok(());
    }
    if unit.overwrite_markers {
        let replacement = build_marker_replacement(mgr, unit, &comparison)?;
        let chapter_count = replacement.chapter_updates.len();
        let chapter_updates_clone = replacement.chapter_updates.clone();
        {
            let store = mgr.store.lock();
            validate_publication_rows(&store, std::slice::from_ref(&unit.row))?;
            validate_publication_rows(&store, &comparison.samples.season_rows)?;
            store.replace_marker_results_batch_checked(&[replacement])?;
        }
        if let Some(service) = mgr.chapter_frames() {
            crate::http::library_chapters::trigger_scene_frames_for_chapter_updates(
                &chapter_updates_clone,
                &service,
            );
        }
        tracing::info!(
            media_id = %unit.row.media_id,
            season,
            replaced_markers = comparison.markers.len(),
            updated_chapter_caches = chapter_count,
            "【片头片尾】已原子替换旧标记和章节缓存，并触发场景帧后台提取"
        );
    } else {
        let updates = {
            let store = mgr.store.lock();
            validate_publication_rows(&store, std::slice::from_ref(&unit.row))?;
            let mut updates = Vec::new();
            for marker in comparison.markers.iter().cloned() {
                updates.extend(persist_marker(
                    &store,
                    &comparison.samples.all_rows,
                    marker,
                    false,
                )?);
            }
            updates
        };
        // Drop Store before enqueueing; the service re-reads it when it runs.
        if let Some(service) = mgr.chapter_frames() {
            crate::http::library_chapters::trigger_scene_frames_for_chapter_updates(
                &updates,
                &service,
            );
        }
    }
    if unit.overwrite_markers {
        log_unmatched_episodes(unit, &comparison);
    }
    log_comparison_complete(unit, &comparison);
    Ok(())
}

/// Call only while holding Store, immediately before publishing the computed result.
pub(super) fn validate_publication_rows(
    store: &Store,
    rows: &[LedgerRow],
) -> Result<(), crate::store::StoreError> {
    for row in rows {
        crate::fingerprint_job::source::ensure_source_available(store, row)
            .map_err(crate::store::StoreError::Missing)?;
    }
    Ok(())
}

fn season_input_unchanged(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
) -> Result<bool, crate::store::StoreError> {
    let digest = season_digest(mgr, unit)?;
    let stored = mgr
        .store
        .lock()
        .season_comparison_digest(&unit.row.media_id.to_string(), unit.row.season.unwrap_or(1))?;
    Ok(stored.as_deref() == Some(digest.as_str()))
}

fn remember_season_digest(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
) -> Result<(), crate::store::StoreError> {
    let digest = season_digest(mgr, unit)?;
    let store = mgr.store.lock();
    validate_publication_rows(&store, std::slice::from_ref(&unit.row))?;
    store.save_season_comparison_digest(
        &unit.row.media_id.to_string(),
        unit.row.season.unwrap_or(1),
        &digest,
        mgr.clock.lock().now_ms(),
    )
}

fn season_digest(mgr: &ProbeManager, unit: &ProbeUnit) -> Result<String, crate::store::StoreError> {
    let samples = load_season_samples(mgr, unit)?;
    let mut parts = Vec::new();
    for (row, words) in &samples.intros {
        parts.push(format!("i:{}:{}", row.id, words.len()));
    }
    for (row, words, offset) in &samples.outros {
        parts.push(format!("o:{}:{}:{}", row.id, words.len(), offset));
    }
    parts.sort();
    Ok(parts.join("|"))
}

pub(super) fn prepare_marker_replacement(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
) -> Result<MarkerResultReplacement, crate::store::StoreError> {
    let comparison = compare_season_fingerprints(mgr, unit)?;
    let replacement = build_marker_replacement(mgr, unit, &comparison)?;
    log_unmatched_episodes(unit, &comparison);
    Ok(replacement)
}

fn compare_season_fingerprints(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
) -> Result<MarkerComparison, crate::store::StoreError> {
    let started_at = Instant::now();
    let season = unit.row.season.unwrap_or(1);
    let samples = load_season_samples(mgr, unit)?;
    tracing::info!(
        media_id = %unit.row.media_id,
        season,
        episode_rows = samples.season_rows.len(),
        intro_samples = samples.intros.len(),
        outro_samples = samples.outros.len(),
        overwrite = unit.overwrite_markers,
        refresh_id = ?unit.marker_refresh_id,
        "【片头片尾】声纹比对开始"
    );
    let intro_sample_episodes = samples
        .intros
        .iter()
        .map(|(row, _)| row.episode.unwrap_or(1))
        .collect();
    let outro_sample_episodes = samples
        .outros
        .iter()
        .map(|(row, _, _)| row.episode.unwrap_or(1))
        .collect();
    let compare_started_at = Instant::now();
    mgr.comparison_runs
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let markers = if samples.intros.len() >= 2 || samples.outros.len() >= 2 {
        crate::fingerprint_job::analyze_fingerprints_with_outro_engine(
            mgr.fingerprint_engine.as_ref(),
            unit.row.media_id,
            season,
            samples.intros.clone(),
            samples.outros.clone(),
        )
    } else {
        Vec::new()
    };
    mgr.timings.record_comparison(
        unit.job_id.as_deref(),
        compare_started_at.elapsed().as_millis() as u64,
    );
    log_comparison_counts(unit, &samples, &markers, compare_started_at);
    log_match_details(unit, &samples.all_rows, &markers);
    Ok(MarkerComparison {
        intro_count: samples.intros.len(),
        outro_count: samples.outros.len(),
        samples,
        markers,
        intro_sample_episodes,
        outro_sample_episodes,
        started_at,
    })
}

fn load_season_samples(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
) -> Result<SeasonFingerprintSamples, crate::store::StoreError> {
    let store = mgr.store.lock();
    let all_rows = store.list_ledger()?;
    let season_rows: Vec<_> = all_rows
        .iter()
        .filter(|row| {
            row.media_id == unit.row.media_id
                && row.season.unwrap_or(1) == unit.row.season.unwrap_or(1)
        })
        .cloned()
        .collect();
    let duration_secs = store
        .get_scrape_config()
        .ok()
        .map(|config| config.effective.fingerprint_duration_secs)
        .unwrap_or(180) as i64;
    let mut intros = Vec::new();
    let mut outros = Vec::new();
    for row in &season_rows {
        let key = row.id.to_string();
        match store.get_fingerprint_cache(&key) {
            Ok(Some(cache)) => {
                let source_version = media_source_version(std::path::Path::new(&row.path));
                let expected_key = fingerprint_cache_key(
                    &source_version,
                    cache.sample_duration_secs,
                    cache.media_duration_ms,
                );
                if cache.cache_key != expected_key
                    || cache.algorithm_version != FINGERPRINT_ALGORITHM_VERSION
                    || i64::from(cache.sample_duration_secs) != duration_secs
                {
                    tracing::warn!(
                        ledger_id = %key,
                        episode = row.episode.unwrap_or(1),
                        cache_key = %cache.cache_key,
                        expected_cache_key = %expected_key,
                        source_version = %source_version,
                        cached_algorithm = cache.algorithm_version,
                        expected_algorithm = FINGERPRINT_ALGORITHM_VERSION,
                        cached_sample_duration_secs = cache.sample_duration_secs,
                        expected_sample_duration_secs = duration_secs,
                        "【片头片尾】跳过身份不匹配的声纹缓存，需重新采集"
                    );
                    continue;
                }
                if !cache.intro.is_empty() {
                    intros.push((row.clone(), cache.intro));
                }
                if let (Some(fingerprint), Some(duration_ms)) =
                    (cache.outro, cache.media_duration_ms)
                {
                    let offset_ms =
                        (duration_ms - i64::from(cache.sample_duration_secs) * 1000).max(0);
                    outros.push((row.clone(), fingerprint, offset_ms));
                }
            }
            Ok(None) => {}
            Err(error) => tracing::error!(
                %error,
                ledger_id = %key,
                "【片头片尾】读取单集声纹缓存失败"
            ),
        }
    }
    Ok(SeasonFingerprintSamples {
        all_rows,
        season_rows,
        intros,
        outros,
    })
}

fn log_comparison_counts(
    unit: &ProbeUnit,
    samples: &SeasonFingerprintSamples,
    markers: &[StoredMediaMarker],
    compare_started_at: Instant,
) {
    let intro_markers = markers
        .iter()
        .filter(|marker| marker.intro_start_ms.is_some() || marker.intro_end_ms.is_some())
        .count();
    let outro_markers = markers
        .iter()
        .filter(|marker| marker.outro_start_ms.is_some() || marker.outro_end_ms.is_some())
        .count();
    tracing::info!(
        media_id = %unit.row.media_id,
        season = unit.row.season.unwrap_or(1),
        intro_samples = samples.intros.len(),
        outro_samples = samples.outros.len(),
        matched_episodes = markers.len(),
        intro_markers,
        outro_markers,
        refresh_id = ?unit.marker_refresh_id,
        elapsed_ms = compare_started_at.elapsed().as_millis() as u64,
        "【片头片尾】声纹比对结束"
    );
}

fn log_match_details(unit: &ProbeUnit, rows: &[LedgerRow], markers: &[StoredMediaMarker]) {
    for marker in markers {
        let path = rows
            .iter()
            .find(|row| {
                row.media_id == marker.media_id
                    && row.season.unwrap_or(1) == marker.season
                    && row.episode.unwrap_or(1) == marker.episode
            })
            .map(|row| row.path.as_str())
            .unwrap_or("");
        if unit.overwrite_markers {
            tracing::info!(
                media_id = %marker.media_id,
                season = marker.season,
                episode = marker.episode,
                path,
                intro_start_ms = ?marker.intro_start_ms,
                intro_end_ms = ?marker.intro_end_ms,
                outro_start_ms = ?marker.outro_start_ms,
                outro_end_ms = ?marker.outro_end_ms,
                "【片头片尾】单集新识别结果"
            );
        } else {
            tracing::debug!(
                media_id = %marker.media_id,
                season = marker.season,
                episode = marker.episode,
                path,
                intro_start_ms = ?marker.intro_start_ms,
                intro_end_ms = ?marker.intro_end_ms,
                outro_start_ms = ?marker.outro_start_ms,
                outro_end_ms = ?marker.outro_end_ms,
                "【片头片尾】单集新识别结果"
            );
        }
    }
}

fn build_marker_replacement(
    mgr: &ProbeManager,
    unit: &ProbeUnit,
    comparison: &MarkerComparison,
) -> Result<MarkerResultReplacement, crate::store::StoreError> {
    let season = unit.row.season.unwrap_or(1);
    let store = mgr.store.lock();
    let mut chapter_updates = Vec::with_capacity(comparison.samples.season_rows.len());
    for row in &comparison.samples.season_rows {
        let matched = comparison
            .markers
            .iter()
            .find(|candidate| candidate.episode == row.episode.unwrap_or(1));
        let intro = matched.and_then(|candidate| {
            match (candidate.intro_start_ms, candidate.intro_end_ms) {
                (Some(start), Some(end)) if end > start => Some((start, end)),
                _ => None,
            }
        });
        let outro = matched.and_then(|candidate| {
            match (candidate.outro_start_ms, candidate.outro_end_ms) {
                (Some(start), Some(end)) if end > start => Some((start, end)),
                (Some(start), None) => Some((start, start + 60_000)),
                _ => None,
            }
        });
        let existing = store
            .get_cached_chapters(&row.id.to_string())?
            .unwrap_or_default();
        let complete = marker::build_complete_timeline_chapters(&existing, intro, outro, None);
        chapter_updates.push((row.id.to_string(), complete));
    }
    Ok(MarkerResultReplacement {
        media_id: unit.row.media_id,
        season,
        markers: comparison.markers.clone(),
        chapter_updates,
    })
}

fn log_unmatched_episodes(unit: &ProbeUnit, comparison: &MarkerComparison) {
    let matched = comparison
        .markers
        .iter()
        .map(|marker| marker.episode)
        .collect::<HashSet<_>>();
    for row in &comparison.samples.season_rows {
        let episode = row.episode.unwrap_or(1);
        if matched.contains(&episode) {
            continue;
        }
        tracing::info!(
            media_id = %unit.row.media_id,
            season = unit.row.season.unwrap_or(1),
            episode,
            path = %row.path,
            had_intro_sample = comparison.intro_sample_episodes.contains(&episode),
            had_outro_sample = comparison.outro_sample_episodes.contains(&episode),
            "【片头片尾】该集本轮未匹配到片头或片尾，刷新成功后移除旧结果"
        );
    }
}

fn log_comparison_complete(unit: &ProbeUnit, comparison: &MarkerComparison) {
    let intro_markers = comparison
        .markers
        .iter()
        .filter(|marker| marker.intro_start_ms.is_some() || marker.intro_end_ms.is_some())
        .count();
    let outro_markers = comparison
        .markers
        .iter()
        .filter(|marker| marker.outro_start_ms.is_some() || marker.outro_end_ms.is_some())
        .count();
    tracing::info!(
        media_id = %unit.row.media_id,
        season = unit.row.season.unwrap_or(1),
        episodes = comparison.samples.season_rows.len(),
        matched_episodes = comparison.markers.len(),
        intro_markers,
        outro_markers,
        replaced = unit.overwrite_markers,
        refresh_id = ?unit.marker_refresh_id,
        elapsed_ms = comparison.started_at.elapsed().as_millis() as u64,
        "【片头片尾】声纹比对和标记写入完成"
    );
}

pub(super) fn persist_marker(
    store: &Store,
    rows: &[LedgerRow],
    mut marker: crate::store::StoredMediaMarker,
    overwrite_markers: bool,
) -> Result<Vec<(String, Vec<library::ChapterMarker>)>, crate::store::StoreError> {
    let mut chapter_updates = Vec::new();
    let started_at = Instant::now();
    let mut live_rows = Vec::new();
    for row in rows.iter().filter(|row| {
        row.media_id == marker.media_id
            && row.season.unwrap_or(1) == marker.season
            && row.episode.unwrap_or(1) == marker.episode
    }) {
        match crate::fingerprint_job::source::ensure_source_available(store, row) {
            Ok(()) => live_rows.push(row),
            Err(reason) if crate::fingerprint_job::source::is_file_deleted(&reason) => {
                tracing::info!(ledger_id = %row.id, %reason, "跳过已删除或冻结的标记发布目标");
            }
            Err(reason) => return Err(crate::store::StoreError::Missing(reason)),
        }
    }
    if live_rows.is_empty() {
        return Ok(chapter_updates);
    }
    if let Some(existing) =
        store.get_media_marker(marker.media_id, Some(marker.season), Some(marker.episode))?
    {
        if !overwrite_markers {
            if existing.locked {
                marker.intro_start_ms = existing.intro_start_ms;
                marker.intro_end_ms = existing.intro_end_ms;
                marker.outro_start_ms = existing.outro_start_ms;
                marker.outro_end_ms = existing.outro_end_ms;
                marker.source = existing.source;
            } else {
                marker.intro_start_ms = marker.intro_start_ms.or(existing.intro_start_ms);
                marker.intro_end_ms = marker.intro_end_ms.or(existing.intro_end_ms);
                marker.outro_start_ms = marker.outro_start_ms.or(existing.outro_start_ms);
                marker.outro_end_ms = marker.outro_end_ms.or(existing.outro_end_ms);
            }
        }
        marker.locked = existing.locked && !overwrite_markers;
    }
    store.put_media_marker(&marker)?;
    for row in live_rows {
        let intro = match (marker.intro_start_ms, marker.intro_end_ms) {
            (Some(s), Some(e)) if e > s => Some((s, e)),
            _ => None,
        };
        let outro = match (marker.outro_start_ms, marker.outro_end_ms) {
            (Some(s), Some(e)) if e > s => Some((s, e)),
            (Some(s), None) => Some((s, s + 60_000)),
            _ => None,
        };
        let existing_chapters = store
            .get_cached_chapters(&row.id.to_string())?
            .unwrap_or_default();
        let complete =
            marker::build_complete_timeline_chapters(&existing_chapters, intro, outro, None);
        store.put_cached_chapters(&row.id.to_string(), &complete)?;
        // 返回给调用方在放下 Store 锁之后抓帧。这里若直接抓，会在持锁时
        // 再次读取媒体库开关，和普通声纹入库的外层锁自死锁。
        chapter_updates.push((row.id.to_string(), complete.clone()));
        tracing::debug!(
            media_id = %marker.media_id,
            season = marker.season,
            episode = marker.episode,
            ledger_id = %row.id,
            path = %row.path,
            intro = intro.is_some(),
            outro = outro.is_some(),
            chapter_count = complete.len(),
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            "【片头片尾】单集章节缓存写入结束"
        );
    }
    Ok(chapter_updates)
}
