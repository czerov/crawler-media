//! Bounded probe dispatch.
//!
//! Channels carry [`ProbeDispatchId`] only. The durable queue stays in the
//! store; workers load one snapshot, claim it, and refill the window. A full
//! fingerprint window leaves the unit queued instead of finishing it failed.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use store::{ProbeDispatchId, ProbeDispatchStage};
use tokio::sync::mpsc;

use super::{ProbeManager, ProbeUnit, probe};

pub(super) const METADATA_WINDOW: usize = 64;
pub(super) const FINGERPRINT_WINDOW: usize = 16;

pub(super) struct DispatchChannels {
    pub(super) metadata_tx: mpsc::Sender<ProbeDispatchId>,
    pub(super) metadata_rx: mpsc::Receiver<ProbeDispatchId>,
    pub(super) fingerprint_tx: mpsc::Sender<(ProbeDispatchId, probe::FingerprintWork)>,
    pub(super) fingerprint_rx: mpsc::Receiver<(ProbeDispatchId, probe::FingerprintWork)>,
}

pub(super) fn channels() -> DispatchChannels {
    let (metadata_tx, metadata_rx) = mpsc::channel(METADATA_WINDOW);
    let (fingerprint_tx, fingerprint_rx) = mpsc::channel(FINGERPRINT_WINDOW);
    DispatchChannels {
        metadata_tx,
        metadata_rx,
        fingerprint_tx,
        fingerprint_rx,
    }
}

impl ProbeManager {
    /// How many ledger ids are queued or running in this process.
    pub fn queued_ledger_count(&self) -> usize {
        self.seen.lock().len()
    }

    pub fn worker_count(&self) -> usize {
        self.workers.lock().len()
    }

    pub(super) async fn shutdown_dispatch(&self) -> Result<(), String> {
        if !self.workers_started.swap(false, Ordering::SeqCst) {
            self.workers.lock().clear();
            return Ok(());
        }
        self.shutting_down.store(true, Ordering::SeqCst);
        self.wake.notify_waiters();
        drop(self.metadata_rx.lock().take());
        drop(self.fingerprint_rx.lock().take());
        let handles = std::mem::take(&mut *self.workers.lock());
        for handle in handles {
            handle.abort();
            let _ = handle.await;
        }
        self.seen.lock().clear();
        self.metadata_pending.store(0, Ordering::SeqCst);
        self.shutting_down.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Queue one metadata id. A full window leaves the unit durable and queued.
    pub(super) fn queue_metadata(&self, unit: ProbeUnit) -> Result<(), ProbeUnit> {
        let Some(job_id) = unit.job_id.clone() else {
            return Err(unit);
        };
        let id = ProbeDispatchId {
            job_id,
            ledger_id: unit.row.id.to_string(),
            stage: ProbeDispatchStage::Metadata,
        };
        self.offer(self.metadata_tx.clone(), id, unit)
    }

    /// Persist the metadata result as fingerprint work, then offer its id.
    ///
    /// A full or closed fingerprint window keeps the unit `queued` and wakes
    /// the dispatcher. It does not finish the unit failed.
    pub(super) fn enqueue_fingerprint_after_metadata(
        &self,
        mut work: probe::FingerprintWork,
    ) -> bool {
        let ledger_id = work.unit.row.id.to_string();
        let Some(current_job_id) = work.unit.job_id.clone() else {
            tracing::error!(ledger_id, "媒体信息已完成，但声纹任务缺少持久化任务 ID");
            self.seen.lock().remove(&ledger_id);
            return false;
        };
        let current_job = self.store.lock().get_probe_job(&current_job_id);
        let current_job = match current_job {
            Ok(Some(job)) => job,
            Ok(None) => {
                tracing::error!(ledger_id, job_id = %current_job_id, "媒体信息任务已不存在，无法转入声纹");
                self.seen.lock().remove(&ledger_id);
                return false;
            }
            Err(error) => {
                tracing::error!(%error, ledger_id, "读取媒体信息任务失败，声纹保持排队");
                self.wake.notify_waiters();
                return false;
            }
        };

        if current_job.kind == "media_probe" {
            self.finish_persisted_unit(&work.unit, true);
            let metadata_succeeded = match self.store.lock().get_probe_job(&current_job_id) {
                Ok(Some(job)) => job.succeeded > 0 || job.status == "succeeded",
                Ok(None) => false,
                Err(error) => {
                    tracing::error!(%error, ledger_id, "确认媒体信息任务结果失败");
                    false
                }
            };
            if !metadata_succeeded {
                self.seen.lock().remove(&ledger_id);
                return false;
            }
            let Some(fingerprint_job_id) = self.persist_single_unit(&work.unit, "fingerprint_probe")
            else {
                tracing::error!(ledger_id, "创建声纹任务失败，工作保持可恢复");
                self.seen.lock().remove(&ledger_id);
                self.wake.notify_waiters();
                return false;
            };
            work.unit.job_id = Some(fingerprint_job_id);
            work.start_job = true;
        } else if !self.advance_running_unit(&current_job_id, &ledger_id) {
            tracing::warn!(ledger_id, job_id = %current_job_id, "声纹阶段未持久化，保持原任务可恢复");
            self.wake.notify_waiters();
            return false;
        }

        let Some(job_id) = work.unit.job_id.clone() else {
            return false;
        };
        let id = ProbeDispatchId {
            job_id,
            ledger_id: ledger_id.clone(),
            stage: ProbeDispatchStage::Fingerprint,
        };
        match self.fingerprint_tx.try_send((id, work)) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::info!(
                    ledger_id,
                    "声纹窗口已满，已完成的媒体信息保持排队等待下一次派发"
                );
                self.seen.lock().remove(&ledger_id);
                self.wake.notify_waiters();
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::info!(ledger_id, "声纹 worker 已停止，已完成的媒体信息保持排队");
                self.seen.lock().remove(&ledger_id);
                false
            }
        }
    }

    fn advance_running_unit(&self, job_id: &str, ledger_id: &str) -> bool {
        match self
            .store
            .lock()
            .advance_probe_unit_to_fingerprint(job_id, ledger_id)
        {
            Ok(true) => true,
            Ok(false) => false,
            Err(error) => {
                tracing::error!(%error, job_id, ledger_id, "持久化声纹阶段失败");
                false
            }
        }
    }

    fn offer(
        &self,
        tx: mpsc::Sender<ProbeDispatchId>,
        id: ProbeDispatchId,
        unit: ProbeUnit,
    ) -> Result<(), ProbeUnit> {
        match tx.try_send(id) {
            Ok(()) => {
                self.metadata_pending.fetch_add(1, Ordering::AcqRel);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::info!(
                    ledger_id = %unit.row.id,
                    "探测窗口已满，任务保持数据库排队"
                );
                Err(unit)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(unit),
        }
    }

    /// Fill both windows from durable pending ids. Seen stays at window + active.
    pub(super) fn fill_dispatch_windows(&self) {
        if self.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        self.fill_metadata();
        self.fill_fingerprint();
    }

    fn fill_metadata(&self) {
        let tx = &self.metadata_tx;
        let free = tx.capacity();
        if free == 0 {
            return;
        }
        let ids = match self
            .store
            .lock()
            .pending_probe_dispatch_ids(ProbeDispatchStage::Metadata, free)
        {
            Ok(ids) => ids,
            Err(error) => {
                tracing::error!(%error, "读取媒体信息派发窗口失败");
                return;
            }
        };
        let mut seen = self.seen.lock();
        for id in ids {
            if !seen.insert(id.ledger_id.clone()) {
                continue;
            }
            if tx.try_send(id.clone()).is_err() {
                seen.remove(&id.ledger_id);
                break;
            }
            self.metadata_pending.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn fill_fingerprint(&self) {
        let tx = &self.fingerprint_tx;
        let free = tx.capacity();
        if free == 0 {
            return;
        }
        let ids = match self
            .store
            .lock()
            .pending_probe_dispatch_ids(ProbeDispatchStage::Fingerprint, free)
        {
            Ok(ids) => ids,
            Err(error) => {
                tracing::error!(%error, "读取声纹派发窗口失败");
                return;
            }
        };
        drop(self.seen.lock());
        let mut ready = Vec::new();
        for id in ids {
            if self.seen.lock().contains(&id.ledger_id) {
                continue;
            }
            if let Some((_, work)) = self.load_dispatch_unit(&id) {
                ready.push((id, work));
            }
        }
        let mut seen = self.seen.lock();
        for (id, work) in ready {
            if !seen.insert(id.ledger_id.clone()) {
                continue;
            }
            if tx.try_send((id.clone(), work)).is_err() {
                seen.remove(&id.ledger_id);
                break;
            }
        }
    }

    fn release_unclaimed_window(&self) {
        let mut seen = self.seen.lock();
        while let Ok(id) = self.metadata_tx.try_reserve() {
            drop(id);
            break;
        }
        // Receivers are gone with the workers. Drop whatever ids are still
        // sitting in the channels by rebuilding is not available; the workers
        // drain them. Clear the in-memory set so a later start refills.
        seen.clear();
        self.metadata_pending.store(0, Ordering::SeqCst);
    }

    pub(super) fn load_dispatch_unit(
        &self,
        id: &ProbeDispatchId,
    ) -> Option<(ProbeUnit, probe::FingerprintWork)> {
        self.materialize_dispatch(id, true)
    }

    pub(super) fn load_dispatch_unit_inner(
        &self,
        id: &ProbeDispatchId,
        queued_only: bool,
    ) -> Option<(ProbeUnit, probe::FingerprintWork)> {
        self.materialize_dispatch(id, queued_only)
    }

    fn materialize_dispatch(
        &self,
        id: &ProbeDispatchId,
        queued_only: bool,
    ) -> Option<(ProbeUnit, probe::FingerprintWork)> {
        let snapshot = match self.store.lock().load_probe_dispatch_snapshot(id) {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => {
                self.seen.lock().remove(&id.ledger_id);
                return None;
            }
            Err(error) => {
                tracing::error!(%error, ledger_id = %id.ledger_id, "读取探测派发快照失败");
                self.seen.lock().remove(&id.ledger_id);
                self.wake.notify_waiters();
                return None;
            }
        };
        let acceptable = if queued_only {
            snapshot.unit.status == "queued"
        } else {
            snapshot.unit.status == "queued" || snapshot.unit.status == "running"
        };
        if snapshot.unit.stage != id.stage || !acceptable {
            self.seen.lock().remove(&id.ledger_id);
            return None;
        }
        let kind = match snapshot.unit.kind.parse::<domain::MediaKind>() {
            Ok(kind) => kind,
            Err(_) => {
                tracing::error!(ledger_id = %id.ledger_id, kind = %snapshot.unit.kind, "派发单元媒体类型无效");
                let _ = self.store.lock().finish_probe_unit(
                    &id.job_id,
                    &id.ledger_id,
                    false,
                    Some("派发时媒体类型无效"),
                );
                self.seen.lock().remove(&id.ledger_id);
                return None;
            }
        };
        let unit = ProbeUnit {
            row: snapshot.row,
            kind,
            force_fingerprint: snapshot.unit.force_fingerprint,
            reuse_fingerprint_cache: snapshot.unit.reuse_fingerprint_cache,
            overwrite_markers: snapshot.unit.overwrite_markers,
            reuse_media_info_cache: snapshot.unit.reuse_media_info_cache,
            marker_refresh_id: (snapshot.job.kind == "marker_refresh").then(|| {
                self.next_marker_refresh_id
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            }),
            job_id: Some(snapshot.job.id),
        };
        let work = probe::FingerprintWork {
            unit: unit.clone(),
            media_duration_ms: None,
            source_version: snapshot.source_version,
            duration_secs: 180,
            start_job: true,
        };
        Some((unit, work))
    }
}


