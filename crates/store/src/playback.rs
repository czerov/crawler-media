//! Playback activity store: per-unit progress/marks, live sessions, play
//! logs, and quality metrics. Units use (-1, -1) as the whole-media sentinel
//! (movies / series-level resume), matching the frontend's PlaybackUnit.

use std::str::FromStr;

use domain::{MediaId, UserId};

use super::{Store, StoreError};

pub mod session_ids;
pub use session_ids::{PlaySessionBinding, PlaySessionIdState};
pub const UNIT_WHOLE: i32 = -1;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnitState {
    pub position_ms: i64,
    pub played: bool,
    pub favorite: bool,
    pub duration_ms: Option<i64>,
    pub audio_track: Option<String>,
    pub subtitle_track: Option<String>,
    pub play_count: i64,
    pub updated_at: i64,
    /// When this unit was last favourited. Zero means never favourited, or
    /// favourited before this clock existed (those rows keep `updated_at`).
    pub favorite_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitRow {
    pub season: i32,
    pub episode: i32,
    pub played: bool,
    pub favorite: bool,
    pub updated_at: i64,
    pub favorite_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    pub device_id: String,
    pub user_id: UserId,
    pub media_id: MediaId,
    pub season: Option<i32>,
    pub episode: Option<i32>,
    pub client: Option<String>,
    pub device_name: Option<String>,
    pub client_version: Option<String>,
    pub play_method: String,
    pub position_ms: i64,
    pub start_position_ms: i64,
    pub duration_ms: Option<i64>,
    pub paused: bool,
    pub watched_ms: i64,
    pub rate_bps: i64,
    pub bytes_sent: i64,
    pub connections: i32,
    pub admin_ended: bool,
    pub started_at: i64,
    pub last_report_at: i64,
    pub ledger_id: Option<String>,
    pub play_session_id: Option<String>,
    pub last_report_at_ms: i64,
    pub last_event_sequence: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayLogRow {
    pub id: String,
    pub user_id: UserId,
    pub media_id: MediaId,
    pub season: Option<i32>,
    pub episode: Option<i32>,
    pub device_id: Option<String>,
    pub client: Option<String>,
    pub device_name: Option<String>,
    pub play_method: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub watched_ms: i64,
    pub start_position_ms: i64,
    pub end_position_ms: i64,
    pub duration_ms: Option<i64>,
    pub completed: bool,
}

fn media_id_from(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<MediaId> {
    MediaId::from_str(&row.get::<_, String>(index)?).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(err))
    })
}

fn user_id_from(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<UserId> {
    UserId::from_str(&row.get::<_, String>(index)?).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(err))
    })
}

mod log_scope;
mod logs;
mod retention;
mod sessions;
mod units;
mod watch_aggregate;

pub use log_scope::{PlaybackLogPage, PlaybackLogScope};
pub use retention::{PlaybackRetentionBatch, PlaybackRetentionPreview};
pub use watch_aggregate::{
    WatchClient, WatchDay, WatchMedia, WatchMember, WatchReport, WatchTotals, WatchWindow,
};

impl UnitState {
    /// Shared Web/Jellyfin completion threshold: 90% or the final 30 seconds.
    pub fn playback_completed(position_ms: i64, duration_ms: Option<i64>) -> bool {
        duration_ms.is_some_and(|duration| {
            duration > 0
                && (i128::from(position_ms) >= i128::from(duration) * 9 / 10
                    || duration.saturating_sub(position_ms) <= 30_000)
        })
    }
}

fn revocable_identity(device_id: &str, play_method: &str) -> bool {
    play_method == "DirectPlay"
        && !device_id.is_empty()
        && device_id != "jellyfin-client"
        && !device_id.starts_with("unidentified:")
}

impl SessionRow {
    pub fn revocable(&self) -> bool {
        revocable_identity(&self.device_id, &self.play_method)
    }
}

impl PlayLogRow {
    pub fn revocable(&self) -> bool {
        self.device_id
            .as_deref()
            .is_some_and(|id| revocable_identity(id, &self.play_method))
    }
}

impl Store {
    /// Commit related Playback writes together; errors retain the prior state.
    pub fn playback_write<T>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        if !self.subscribe.is_autocommit() {
            return operation(self);
        }
        let transaction = self.subscribe.unchecked_transaction()?;
        let result = operation(self)?;
        transaction.commit()?;
        Ok(result)
    }
}
