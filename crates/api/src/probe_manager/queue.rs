use std::sync::atomic::Ordering;

use super::ProbeManager;

impl ProbeManager {
    pub fn metadata_stage_finished(&self) {
        match self
            .metadata_pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
                pending.checked_sub(1)
            }) {
            Ok(1) => {
                self.metadata_idle_generation.fetch_add(1, Ordering::AcqRel);
                self.metadata_idle.notify_waiters();
                if !self.shutting_down.load(Ordering::SeqCst) {
                    self.fill_dispatch_windows();
                    self.wake.notify_waiters();
                }
            }
            Ok(_) => {}
            Err(_) => tracing::error!("媒体信息队列待处理计数异常：完成时计数已为 0"),
        }
    }
}
