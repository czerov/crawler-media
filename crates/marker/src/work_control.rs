//! Shared cancellation signal for media child processes; no API dependencies.
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

#[derive(Clone, Default, Debug)]
pub struct WorkControl {
    cancelled: Arc<AtomicBool>,
}

impl WorkControl {
    pub fn cancel(&self) { self.cancelled.store(true, Ordering::Release); }
    pub fn is_cancelled(&self) -> bool { self.cancelled.load(Ordering::Acquire) }
}
