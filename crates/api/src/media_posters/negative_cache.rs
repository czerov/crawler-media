use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use domain::MediaId;
use super::PosterError;

pub trait PosterClock: Send + Sync {
    fn now(&self) -> Duration;
}

pub struct SystemPosterClock {
    started_at: std::time::Instant,
}

impl SystemPosterClock {
    pub fn new() -> Self {
        Self {
            started_at: std::time::Instant::now(),
        }
    }
}

impl Default for SystemPosterClock {
    fn default() -> Self {
        Self::new()
    }
}

impl PosterClock for SystemPosterClock {
    fn now(&self) -> Duration {
        self.started_at.elapsed()
    }
}

struct NegativeCacheEntry {
    media_id: MediaId,
    error: PosterError,
    inserted_at: Duration,
}

pub struct NegativePosterCache {
    capacity: usize,
    ttl: Duration,
    clock: Arc<dyn PosterClock>,
    entries: VecDeque<NegativeCacheEntry>,
}

impl NegativePosterCache {
    pub fn new(capacity: usize, ttl: Duration, clock: Arc<dyn PosterClock>) -> Self {
        Self {
            capacity,
            ttl,
            clock,
            entries: VecDeque::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn sweep_expired(&mut self, now: Duration) {
        // Protect against clock going backward: if now < inserted_at, do not expire prematurely
        self.entries.retain(|entry| {
            if now < entry.inserted_at {
                true
            } else {
                now - entry.inserted_at < self.ttl
            }
        });
    }

    pub fn get(&mut self, id: MediaId) -> Option<PosterError> {
        let now = self.clock.now();
        self.sweep_expired(now);

        let pos = self.entries.iter().position(|e| e.media_id == id)?;
        let mut entry = self.entries.remove(pos)?;
        let error = entry.error.clone();
        // Move to most recently used (back)
        self.entries.push_back(entry);
        Some(error)
    }

    pub fn insert(&mut self, id: MediaId, error: PosterError) {
        let now = self.clock.now();
        self.sweep_expired(now);

        // If key already present, remove old entry so it can be updated & pushed to back
        if let Some(pos) = self.entries.iter().position(|e| e.media_id == id) {
            self.entries.remove(pos);
        }

        // Evict LRU (front) until under capacity
        while self.entries.len() >= self.capacity && self.capacity > 0 {
            self.entries.pop_front();
        }

        if self.capacity > 0 {
            self.entries.push_back(NegativeCacheEntry {
                media_id: id,
                error,
                inserted_at: now,
            });
        }
    }
}
