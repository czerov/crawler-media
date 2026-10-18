use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use crate::IndexerError;

struct SlotState {
    active: usize,
    active_groups: HashSet<String>,
    waiting: VecDeque<(u64, String)>,
    next_ticket: u64,
}

pub struct BrowserSlots {
    max_sessions: usize,
    state: Mutex<SlotState>,
    cvar: Condvar,
}

pub struct SlotGuard {
    slots: Arc<BrowserSlots>,
    group: String,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let mut state = self.slots.state.lock();
        state.active = state.active.saturating_sub(1);
        state.active_groups.remove(&self.group);
        self.slots.cvar.notify_all();
    }
}

impl BrowserSlots {
    pub fn new(max_sessions: usize) -> Arc<Self> {
        Arc::new(Self {
            max_sessions: max_sessions.clamp(1, 8),
            state: Mutex::new(SlotState {
                active: 0,
                active_groups: HashSet::new(),
                waiting: VecDeque::new(),
                next_ticket: 0,
            }),
            cvar: Condvar::new(),
        })
    }

    pub fn acquire(
        self: &Arc<Self>,
        group_key: &str,
        deadline: Instant,
        queue_cap: Duration,
        min_work: Duration,
    ) -> Result<SlotGuard, IndexerError> {
        let now = Instant::now();
        let work_latest = deadline
            .checked_sub(min_work)
            .ok_or(IndexerError::BrowserBusy)?;
        if now >= work_latest {
            return Err(IndexerError::BrowserBusy);
        }
        let wait_deadline = now
            .checked_add(queue_cap)
            .map(|t| t.min(work_latest))
            .unwrap_or(work_latest);

        let mut state = self.state.lock();
        let ticket = state.next_ticket;
        state.next_ticket += 1;
        state.waiting.push_back((ticket, group_key.to_string()));

        loop {
            // Find the first runnable ticket in FIFO order
            let first_runnable = if state.active < self.max_sessions {
                state
                    .waiting
                    .iter()
                    .find(|(_, g)| !state.active_groups.contains(g))
                    .map(|(t, _)| *t)
            } else {
                None
            };

            if first_runnable == Some(ticket) {
                // Remove self from queue
                state.waiting.retain(|(t, _)| *t != ticket);
                if Instant::now() > work_latest {
                    self.cvar.notify_all();
                    return Err(IndexerError::BrowserBusy);
                }
                state.active += 1;
                state.active_groups.insert(group_key.to_string());
                return Ok(SlotGuard {
                    slots: self.clone(),
                    group: group_key.to_string(),
                });
            }

            let cur_now = Instant::now();
            if cur_now >= wait_deadline {
                state.waiting.retain(|(t, _)| *t != ticket);
                self.cvar.notify_all();
                return Err(IndexerError::BrowserBusy);
            }
            let remaining = wait_deadline.saturating_duration_since(cur_now);
            self.cvar.wait_for(&mut state, remaining);
        }
    }
}
