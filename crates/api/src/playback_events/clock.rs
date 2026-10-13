pub trait PlaybackClock: Send + Sync {
    fn now_ms(&self) -> i64;
}

#[derive(Default)]
pub struct SystemPlaybackClock;

impl PlaybackClock for SystemPlaybackClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

/// Clamped elapsed calculation: [0..=30_000] ms.
pub fn clamp_elapsed(now_ms: i64, last_report_at_ms: i64) -> i64 {
    if last_report_at_ms <= 0 || now_ms <= last_report_at_ms {
        0
    } else {
        (now_ms - last_report_at_ms).min(30_000).max(0)
    }
}
