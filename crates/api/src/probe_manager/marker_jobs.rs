use std::collections::HashSet;

use domain::{MediaId, MediaKind};

use super::{ProbeManager, ProbeUnit, markers};

pub(super) fn apply_marker_refresh(
    mgr: &ProbeManager,
    job: &crate::store::ProbeJob,
    unit: &ProbeUnit,
) {
    let media_id = match uuid::Uuid::parse_str(&job.media_id) {
        Ok(id) => MediaId::from_uuid(id),
        Err(error) => {
            fail_job(mgr, job, "媒体 ID 无效");
            tracing::error!(%error, job_id = %job.id, media_id = %job.media_id, "持久化声纹任务中的媒体 ID 无效");
            return;
        }
    };
    let seasons = match marker_refresh_seasons(mgr, job) {
        Ok(seasons) if !seasons.is_empty() => seasons,
        Ok(_) => {
            fail_job(mgr, job, "刷新任务中没有有效季信息");
            return;
        }
        Err(crate::store::StoreError::Missing(reason))
            if crate::fingerprint_job::source::is_file_deleted(&reason) =>
        {
            mgr.cancel_unit(unit, "file_deleted");
            return;
        }
        Err(error) => {
            fail_job(mgr, job, "读取刷新任务季信息失败");
            tracing::error!(%error, job_id = %job.id, "读取声纹刷新任务季信息失败");
            return;
        }
    };
    let all_rows = mgr.store.lock().list_ledger();
    let all_rows = match all_rows {
        Ok(rows) => rows,
        Err(error) => {
            fail_job(mgr, job, "读取媒体台账失败");
            tracing::error!(%error, job_id = %job.id, "生成声纹刷新结果时读取媒体台账失败");
            return;
        }
    };
    let replacements = match prepare_season_replacements(
        mgr, job, unit, media_id, &seasons, &all_rows,
    ) {
        Ok(replacements) => replacements,
        Err((season, error)) => {
            if matches!(&error, crate::store::StoreError::Missing(reason) if crate::fingerprint_job::source::is_file_deleted(reason))
            {
                mgr.cancel_unit(unit, "file_deleted");
                return;
            }
            fail_job(mgr, job, "片头片尾标记准备失败");
            tracing::error!(%error, job_id = %job.id, %media_id, season, "【片头片尾】准备整条目刷新结果失败，旧标记保持可见");
            return;
        }
    };
    let publication = {
        let store = mgr.store.lock();
        let rows = all_rows
            .iter()
            .filter(|row| row.media_id == media_id && seasons.contains(&row.season.unwrap_or(1)))
            .cloned()
            .collect::<Vec<_>>();
        markers::validate_publication_rows(&store, &rows).and_then(|()| {
            // Validate every original job member under this same lock, not just the
            // anchor or the expanded lease that ended during result preparation.
            validate_refresh_units(&store, &job.id)?;
            store.complete_marker_refresh_checked(&job.id, &replacements)
        })
    };
    if let Err(error) = publication {
        if matches!(&error, crate::store::StoreError::Missing(reason) if crate::fingerprint_job::source::is_file_deleted(reason))
        {
            mgr.cancel_unit(unit, "file_deleted");
            return;
        }
        fail_job(mgr, job, "片头片尾标记写入失败");
        tracing::error!(%error, job_id = %job.id, %media_id, seasons = ?seasons, "【片头片尾】多季原子写入失败，旧标记保持可见");
        return;
    }
    if let Some(service) = mgr.chapter_frames() {
        for rep in &replacements {
            crate::http::library_chapters::trigger_scene_frames_for_chapter_updates(
                &rep.chapter_updates,
                &service,
            );
        }
    }
    log_marker_refresh_success(job, media_id, &seasons);
}

fn validate_refresh_units(
    store: &crate::Store,
    job_id: &str,
) -> Result<(), crate::store::StoreError> {
    for unit in store.probe_job_units(job_id)? {
        let row = store
            .get_ledger(&unit.ledger_id.replace('-', ""))?
            .ok_or_else(|| {
                crate::store::StoreError::Missing(format!(
                    "file_deleted: ledger {}",
                    unit.ledger_id
                ))
            })?;
        markers::validate_publication_rows(store, &[row])?;
    }
    Ok(())
}

fn log_marker_refresh_success(job: &crate::store::ProbeJob, media_id: MediaId, seasons: &[u32]) {
    tracing::info!(
        job_id = %job.id,
        %media_id,
        seasons = ?seasons,
        completed = job.completed,
        episodes = job.total,
        elapsed_ms = job.elapsed_ms(now_ms()),
        "【片头片尾】持久化整条目刷新完成，全部季标记和章节缓存已原子替换"
    );
}

fn marker_refresh_seasons(
    mgr: &ProbeManager,
    job: &crate::store::ProbeJob,
) -> Result<Vec<u32>, crate::store::StoreError> {
    let store = mgr.store.lock();
    let units = store.probe_job_units(&job.id)?;
    let mut seasons = HashSet::new();
    for unit in units {
        let row = store
            .get_ledger(&unit.ledger_id.replace('-', ""))?
            .ok_or_else(|| {
                crate::store::StoreError::Missing(format!(
                    "file_deleted: ledger {}",
                    unit.ledger_id
                ))
            })?;
        crate::fingerprint_job::source::ensure_source_available(&store, &row)
            .map_err(crate::store::StoreError::Missing)?;
        seasons.insert(row.season.unwrap_or(1));
    }
    let mut seasons = seasons.into_iter().collect::<Vec<_>>();
    seasons.sort_unstable();
    Ok(seasons)
}

fn prepare_season_replacements(
    mgr: &ProbeManager,
    job: &crate::store::ProbeJob,
    unit: &ProbeUnit,
    media_id: MediaId,
    seasons: &[u32],
    all_rows: &[domain::LedgerRow],
) -> Result<Vec<crate::store::MarkerResultReplacement>, (u32, crate::store::StoreError)> {
    let sampling_mode = {
        let store = mgr.store.lock();
        crate::scrape_store::ScrapeStoreExt::get_scrape_config(&*store)
            .ok()
            .map(|c| c.effective.fingerprint_sampling_mode)
            .unwrap_or_else(|| "full_window".to_string())
    };

    let mut replacements = Vec::with_capacity(seasons.len());
    for season in seasons {
        let row = all_rows
            .iter()
            .find(|row| row.media_id == media_id && row.season.unwrap_or(1) == *season)
            .cloned()
            .ok_or_else(|| {
                (
                    *season,
                    crate::store::StoreError::Missing(format!("season {season} ledger row")),
                )
            })?;

        let mut replacement =
            if sampling_mode == "adaptive" && tokio::runtime::Handle::try_current().is_ok() {
                let season_units: Vec<ProbeUnit> = all_rows
                    .iter()
                    .filter(|r| r.media_id == media_id && r.season.unwrap_or(1) == *season)
                    .map(|r| {
                        let mut u = unit.clone();
                        u.row = r.clone();
                        u.kind = MediaKind::Tv;
                        u.force_fingerprint = true;
                        u.overwrite_markers = true;
                        u
                    })
                    .collect();

                let handle = tokio::runtime::Handle::current();
                let job_id = job.id.clone();
                let season_val = *season;

                tokio::task::block_in_place(|| {
                    handle.block_on(async {
                        super::season::run_adaptive_season_pipeline(
                            mgr,
                            &job_id,
                            media_id,
                            season_val,
                            &season_units,
                        )
                        .await
                    })
                })
                .map_err(|e| (*season, crate::store::StoreError::Missing(e)))?
            } else {
                let mut anchor_unit = unit.clone();
                anchor_unit.row = row;
                anchor_unit.kind = MediaKind::Tv;
                anchor_unit.force_fingerprint = true;
                anchor_unit.overwrite_markers = true;
                markers::prepare_marker_replacement(mgr, &anchor_unit)
                    .map_err(|error| (*season, error))?
            };

        // The job's membership is what the caller leased and validated, so the
        // publication must cover exactly those rows.
        scope_replacement_to_job(mgr, job, &mut replacement)
            .map_err(|error| (*season, error))?;

        tracing::info!(
            job_id = %job.id,
            %media_id,
            season,
            markers = replacement.markers.len(),
            chapter_caches = replacement.chapter_updates.len(),
            "【片头片尾】本季刷新结果已准备，等待整条目原子写入"
        );
        replacements.push(replacement);
    }
    Ok(replacements)
}

fn scope_replacement_to_job(
    mgr: &ProbeManager,
    job: &crate::store::ProbeJob,
    replacement: &mut crate::store::MarkerResultReplacement,
) -> Result<(), crate::store::StoreError> {
    let units = mgr.store.lock().probe_job_units(&job.id)?;
    let members: std::collections::HashSet<String> =
        units.into_iter().map(|unit| unit.ledger_id).collect();
    let before = replacement.chapter_updates.len();
    replacement
        .chapter_updates
        .retain(|(id, _)| members.contains(id));
    let dropped = before.saturating_sub(replacement.chapter_updates.len());
    if dropped > 0 {
        tracing::info!(
            job_id = %job.id,
            kept = replacement.chapter_updates.len(),
            dropped,
            "【片头片尾】整条目刷新只发布任务范围内的剧集，跳过其他库的同季行"
        );
    }
    Ok(())
}

fn fail_job(mgr: &ProbeManager, job: &crate::store::ProbeJob, message: &str) {
    if let Err(error) = mgr
        .store
        .lock()
        .finish_probe_job(&job.id, false, Some(message))
    {
        tracing::error!(%error, job_id = %job.id, %message, "持久化声纹任务失败状态失败");
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}
