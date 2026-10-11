use domain::MediaKind;
use std::sync::atomic::Ordering;

use super::{ProbeManager, ProbeUnit};

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc;

    #[test]
    fn ordinary_recovery_keeps_other_library_units_queued() {
        for kind in ["probe", "marker_refresh"] {
            let temp = tempfile::tempdir().unwrap();
            let store = Arc::new(Mutex::new(store::Store::open(temp.path()).unwrap()));
            let media = domain::Media {
                id: domain::MediaId::new(),
                kind: MediaKind::Tv,
                title: "Fixture".into(),
                year: None,
                original_title: None,
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            };
            store.lock().insert_media(&media).unwrap();
            let mut libraries = Vec::new();
            let mut ledger_ids = Vec::new();
            for name in ["removed", "retained"] {
                let root = temp.path().join(name);
                libraries.push(
                    store
                        .lock()
                        .create_library(
                            media.kind,
                            name,
                            &[root.to_str().unwrap()],
                            "everyone",
                            true,
                            &[],
                        )
                        .unwrap(),
                );
                let row = domain::LedgerRow {
                    id: domain::LedgerId::new(),
                    media_id: media.id,
                    path: root.join("episode.mkv").to_string_lossy().into_owned(),
                    season: Some(1),
                    episode: Some(1),
                    resolution: None,
                    codec: None,
                    hdr: None,
                    quality_source: domain::QualitySource::Release,
                    confidence: domain::Confidence::High,
                    filter_score: None,
                };
                store.lock().insert_ledger(&row).unwrap();
                ledger_ids.push(row.id.to_string());
            }
            let specs: Vec<_> = ledger_ids
                .iter()
                .map(|id| store::ProbeJobUnitSpec {
                    ledger_id: id,
                    kind: "tv",
                    force_fingerprint: false,
                    reuse_fingerprint_cache: false,
                    overwrite_markers: false,
                    reuse_media_info_cache: true,
                })
                .collect();
            store
                .lock()
                .create_probe_job(
                    "mixed",
                    kind,
                    &media.id.to_string(),
                    Some(1),
                    "mixed",
                    &specs,
                )
                .unwrap();
            store
                .lock()
                .begin_library_removal(&libraries[0].id)
                .unwrap();
            let manager = ProbeManager::new(store.clone());
            let units = store.lock().probe_job_units("mixed").unwrap();
            assert_eq!(
                units
                    .iter()
                    .find(|u| u.ledger_id == ledger_ids[0])
                    .unwrap()
                    .status,
                "cancelled"
            );
            if kind == "marker_refresh" {
                assert!(units.iter().all(|u| u.status == "cancelled"));
                assert!(!manager.is_queued(&ledger_ids[1]));
            } else {
                assert_eq!(
                    units
                        .iter()
                        .find(|u| u.ledger_id == ledger_ids[1])
                        .unwrap()
                        .status,
                    "queued"
                );
                assert!(manager.is_queued(&ledger_ids[1]));
                assert!(
                    store
                        .lock()
                        .get_probe_job("mixed")
                        .unwrap()
                        .unwrap()
                        .is_active()
                );
            }
        }
    }
}

impl ProbeManager {
    pub(crate) fn recover_pending_jobs(&self) {
        let jobs = match self.store.lock().recover_probe_jobs() {
            Ok(jobs) => jobs,
            Err(error) => {
                tracing::error!(%error, "读取待恢复媒体探测任务失败");
                return;
            }
        };
        for (job, units) in jobs {
            let removed_units = self.removed_recovery_units(&units);
            if !removed_units.is_empty() && job.kind == "marker_refresh" {
                if let Err(error) = self
                    .store
                    .lock()
                    .cancel_probe_job(&job.id, "Library removed")
                {
                    tracing::error!(%error, job_id=%job.id, "恢复时取消删除库任务失败");
                }
                continue;
            }
            tracing::info!(
                job_id = %job.id,
                media_id = %job.media_id,
                status = %job.status,
                pending = units.iter().filter(|unit| unit.status == "queued").count(),
                completed = job.completed,
                total = job.total,
                "【探测任务】从数据库恢复未完成任务"
            );
            // A crash may occur after the unit cancellation commits and before the
            // parent job cancellation. An atomic refresh must never resume that batch.
            if job.kind == "marker_refresh" && units.iter().any(|unit| unit.status == "cancelled") {
                let reason = units
                    .iter()
                    .find(|unit| unit.status == "cancelled")
                    .and_then(|unit| unit.error.as_deref())
                    .unwrap_or("file_deleted");
                match self.store.lock().cancel_probe_job(&job.id, reason) {
                    Ok(()) => tracing::info!(job_id = %job.id, reason,
                        "恢复已中止的原子刷新，释放任务范围"),
                    Err(error) => tracing::error!(%error, job_id = %job.id,
                        "恢复原子刷新取消状态失败"),
                }
                continue;
            }
            self.recover_retained_units(&job, units, &removed_units);
            let refreshed = self.store.lock().get_probe_job(&job.id);
            let refreshed = match refreshed {
                Ok(Some(refreshed)) => refreshed,
                Ok(None) => {
                    tracing::error!(job_id = %job.id, "恢复探测任务后数据库中找不到任务记录");
                    continue;
                }
                Err(error) => {
                    tracing::error!(%error, job_id = %job.id, "恢复探测任务后读取任务状态失败");
                    match self
                        .store
                        .lock()
                        .fail_completed_probe_job(&job.id, "重启恢复时无法读取已完成任务状态")
                    {
                        Ok(true) => {
                            tracing::error!(job_id = %job.id, "已将无法确认状态的完成任务标记失败并释放任务范围")
                        }
                        Ok(false) => {
                            tracing::warn!(job_id = %job.id, "未终止探测任务：任务可能仍有待处理单元或已被其他流程终止")
                        }
                        Err(finish_error) => {
                            tracing::error!(%finish_error, job_id = %job.id, "读取任务状态失败后持久化失败状态也失败")
                        }
                    }
                    continue;
                }
            };
            if refreshed.completed >= refreshed.total && refreshed.is_active() {
                if refreshed.failed > 0 {
                    if let Err(error) = self.store.lock().finish_probe_job(
                        &job.id,
                        false,
                        refreshed.error.as_deref(),
                    ) {
                        tracing::error!(%error, job_id = %job.id, "持久化恢复任务的失败状态失败");
                    }
                } else if refreshed.kind == "marker_refresh" {
                    match self.recovered_marker_refresh_unit(&refreshed) {
                        Ok(unit) => {
                            // 不在构造期间同步执行整季计算，避免等待尚未启动的 worker 导致死锁。
                            // 暂存到 recovered_marker_refreshes，在 try_start_workers 启动 worker 后异步处理。
                            self.recovered_marker_refreshes
                                .lock()
                                .push((refreshed, unit));
                        }
                        Err(crate::store::StoreError::Missing(reason))
                            if crate::fingerprint_job::source::is_file_deleted(&reason) =>
                        {
                            if let Err(error) =
                                self.store.lock().cancel_probe_job(&job.id, "file_deleted")
                            {
                                tracing::error!(%error, job_id = %job.id, "恢复时持久化任务取消状态失败");
                            } else {
                                tracing::info!(job_id = %job.id, reason = "file_deleted", "源文件已删除，取消待发布的刷新任务");
                            }
                        }
                        Err(error) => {
                            if let Err(finish_error) = self.store.lock().finish_probe_job(
                                &job.id,
                                false,
                                Some("重启恢复时无法读取任务台账"),
                            ) {
                                tracing::error!(%finish_error, job_id = %job.id, "持久化无法恢复的片头片尾任务失败状态失败");
                            }
                            tracing::error!(%error, job_id = %job.id, "完成的片头片尾任务无法恢复，已标记失败并释放任务范围");
                        }
                    }
                } else if let Err(error) = self.store.lock().finish_probe_job(&job.id, true, None) {
                    tracing::error!(%error, job_id = %job.id, "持久化恢复任务的成功状态失败");
                }
            }
        }
    }

    fn recover_retained_units(
        &self,
        job: &crate::store::ProbeJob,
        units: Vec<crate::store::ProbeJobUnit>,
        removed: &[String],
    ) {
        for ledger_id in removed {
            if let Err(error) = self.store.lock().cancel_probe_unit(&job.id, ledger_id, "Library removed") {
                tracing::error!(%error, job_id = %job.id, %ledger_id, "恢复时取消删除库单元失败");
            }
        }
        let retained: Vec<_> = units.into_iter().filter(|unit| !removed.contains(&unit.ledger_id)).collect();
        self.recover_job_units(job, &retained);
    }

    fn removed_recovery_units(&self, units: &[crate::store::ProbeJobUnit]) -> Vec<String> {
        let store = self.store.lock();
        units
            .iter()
            .filter(|unit| {
                let row = store.get_ledger(&unit.ledger_id).ok().flatten();
                row.and_then(|row| {
                    let media = store.get_media(row.media_id).ok().flatten()?;
                    store
                        .library_for_path_strict(std::path::Path::new(&row.path), media.kind)
                        .ok()
                        .flatten()
                })
                .is_some_and(|owner| !store.library_accepts_work(&owner.id).unwrap_or(false))
            })
            .map(|unit| unit.ledger_id.clone())
            .collect()
    }

    fn recover_job_units(
        &self,
        job: &crate::store::ProbeJob,
        units: &[crate::store::ProbeJobUnit],
    ) {
        for persisted in units.iter().filter(|unit| unit.status == "queued") {
            if self
                .store
                .lock()
                .get_probe_job(&job.id)
                .ok()
                .flatten()
                .is_some_and(|job| !job.is_active())
            {
                break;
            }
            let row = self
                .store
                .lock()
                .get_ledger(&persisted.ledger_id.replace('-', ""));
            let row = match row {
                Ok(Some(row)) => row,
                Ok(None) => {
                    let result = {
                        let store = self.store.lock();
                        store
                            .cancel_probe_unit(&job.id, &persisted.ledger_id, "file_deleted")
                            .and_then(|(_, _, _)| {
                                if job.kind == "marker_refresh" {
                                    store.cancel_probe_job(&job.id, "file_deleted")?;
                                }
                                Ok(())
                            })
                    };
                    if let Err(error) = result {
                        tracing::error!(%error, job_id = %job.id, "恢复时取消已删除文件任务失败");
                    }
                    continue;
                }
                Err(error) => {
                    tracing::error!(%error, job_id = %job.id, ledger_id = %persisted.ledger_id, "恢复探测任务读取台账失败");
                    if let Err(finish_error) = self.store.lock().finish_probe_unit(
                        &job.id,
                        &persisted.ledger_id,
                        false,
                        Some("恢复任务时读取媒体台账失败"),
                    ) {
                        tracing::error!(%finish_error, job_id = %job.id, ledger_id = %persisted.ledger_id, "持久化恢复失败的任务单元失败");
                    }
                    continue;
                }
            };
            let kind = match persisted.kind.parse::<MediaKind>() {
                Ok(kind) => kind,
                Err(kind) => {
                    tracing::error!(job_id = %job.id, ledger_id = %persisted.ledger_id, %kind, "恢复探测任务媒体类型无效");
                    let _ = self.store.lock().finish_probe_unit(
                        &job.id,
                        &persisted.ledger_id,
                        false,
                        Some("恢复任务时媒体类型无效"),
                    );
                    continue;
                }
            };
            self.seen.lock().insert(persisted.ledger_id.clone());
            let unit = ProbeUnit {
                row,
                kind,
                force_fingerprint: persisted.force_fingerprint,
                reuse_fingerprint_cache: persisted.reuse_fingerprint_cache,
                overwrite_markers: persisted.overwrite_markers,
                reuse_media_info_cache: persisted.reuse_media_info_cache,
                marker_refresh_id: (job.kind == "marker_refresh")
                    .then(|| self.next_marker_refresh_id.fetch_add(1, Ordering::Relaxed)),
                job_id: Some(job.id.clone()),
            };
            if self.queue_metadata(unit).is_err() {
                self.seen.lock().remove(&persisted.ledger_id);
                tracing::error!(job_id = %job.id, ledger_id = %persisted.ledger_id, "恢复探测任务加入队列失败");
                let _ = self.store.lock().finish_probe_unit(
                    &job.id,
                    &persisted.ledger_id,
                    false,
                    Some("恢复任务加入内存队列失败"),
                );
            }
        }
    }

    fn recovered_marker_refresh_unit(
        &self,
        job: &crate::store::ProbeJob,
    ) -> Result<ProbeUnit, crate::store::StoreError> {
        let store = self.store.lock();
        let persisted = store
            .probe_job_units(&job.id)?
            .into_iter()
            .next()
            .ok_or_else(|| {
                crate::store::StoreError::Missing(format!("units for job {}", job.id))
            })?;
        let row = store
            .get_ledger(&persisted.ledger_id.replace('-', ""))?
            .ok_or_else(|| {
                crate::store::StoreError::Missing(format!(
                    "file_deleted: ledger {}",
                    persisted.ledger_id
                ))
            })?;
        let kind = persisted
            .kind
            .parse::<MediaKind>()
            .map_err(|_| crate::store::StoreError::Missing(format!("kind {}", persisted.kind)))?;
        Ok(ProbeUnit {
            row,
            kind,
            force_fingerprint: persisted.force_fingerprint,
            reuse_fingerprint_cache: persisted.reuse_fingerprint_cache,
            overwrite_markers: persisted.overwrite_markers,
            reuse_media_info_cache: persisted.reuse_media_info_cache,
            marker_refresh_id: Some(self.next_marker_refresh_id.fetch_add(1, Ordering::Relaxed)),
            job_id: Some(job.id.clone()),
        })
    }
}
