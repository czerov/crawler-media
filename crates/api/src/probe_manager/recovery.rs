use domain::MediaKind;
use std::sync::atomic::Ordering;

use super::{ProbeManager, ProbeUnit};

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
            let deleting = {
                let store = self.store.lock();
                units.iter().any(|unit| {
                    let row = store.get_ledger(&unit.ledger_id).ok().flatten();
                    row.and_then(|row| {
                        let media = store.get_media(row.media_id).ok().flatten()?;
                        store.library_for_path_strict(std::path::Path::new(&row.path), media.kind).ok().flatten()
                    }).is_some_and(|owner| !store.library_accepts_work(&owner.id).unwrap_or(false))
                })
            };
            if deleting {
                if let Err(error) = self.store.lock().cancel_probe_job(&job.id, "Library removed") {
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
            self.recover_job_units(&job, &units);
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
                            .and_then(|(_, _, all_done)| {
                                if job.kind == "marker_refresh" || all_done {
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
