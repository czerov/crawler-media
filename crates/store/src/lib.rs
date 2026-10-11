//! System-of-record store for crawler-media (ADR-0008).
//! Backed by 4 SQLite files in the data directory.

mod app_write;
mod catalog;
mod collections;
mod downloaders;
mod fingerprint_attempts;
mod fingerprint_cache;
mod fingerprint_models;
mod fingerprint_samples;
mod ledger;
mod ledger_quality;
mod legacy_recycle;
mod libraries;
mod library;
mod library_deletion;
pub use library_deletion::LibraryRemoval;
pub mod library_defaults;
mod library_paths;
mod maps;
mod media;
mod open;
mod playback;
mod playback_devices;
mod probe_tasks;
mod roots;
mod schema;
mod scanned_ledger;
mod sites;
mod subscribe_wanted;
mod subscribe_library;
mod subscribes;
mod users;

pub mod password;
pub mod probe_state;

pub use catalog::CatalogCacheRow;
pub use downloaders::DownloaderRow;
pub use fingerprint_attempts::StoredFingerprintAttempt;
pub use fingerprint_cache::{FingerprintCacheEntry, MediaInfoCacheVersion};
pub use fingerprint_models::{StoredFingerprintModel, StoredFingerprintModelMember};
pub use fingerprint_samples::{FingerprintSampleQuery, StoredFingerprintSample};
pub use libraries::Library;
pub use library::{MarkerResultReplacement, StoredMediaMarker};
pub use playback::{PlayLogRow, SessionRow, UNIT_WHOLE, UnitRow, UnitState};
pub use probe_state::{
    retry_delay_ms, ProbeStage, ProbeStageCompletion, ProbeStageKey, ProbeStageState, ProbeStageStatus,
};
pub use probe_tasks::{ProbeJob, ProbeJobUnit, ProbeJobUnitSpec};
pub use roots::LibraryRoot;
pub use subscribe_wanted::WantedHistory;
pub use subscribes::PendingDownload;

use std::path::PathBuf;

use rusqlite::Connection;

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// RFC3339 UTC string for the current wall-clock time (second precision).
pub fn now_rfc3339() -> String {
    rfc3339_from_secs(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    )
}

/// Format a Unix timestamp as RFC3339 UTC (`YYYY-MM-DDTHH:MM:SSZ`), hand-rolled
/// to avoid a chrono dependency (civil-from-days, valid for our range).
pub fn rfc3339_from_secs(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let h = rem / 3600;
    let m = (rem % 3600) / 60;
    let sec = rem % 60;
    let month_days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut year = 1970i64;
    let mut d = days;
    loop {
        let ydays = if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
            366
        } else {
            365
        };
        if d < ydays {
            break;
        }
        d -= ydays;
        year += 1;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let mut month = 0usize;
    let mut day = d;
    for (i, mdays) in month_days.iter().enumerate() {
        let dim = if i == 1 && leap { 29 } else { *mdays };
        if day < dim {
            month = i + 1;
            break;
        }
        day -= dim;
    }
    format!(
        "{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{sec:02}Z",
        day = day + 1
    )
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid media kind: {0}")]
    MediaKind(String),
    #[error("invalid fetch mode: {0}")]
    FetchMode(String),
    #[error("invalid uuid: {0}")]
    Uuid(#[from] uuid::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("missing {0}")]
    Missing(String),
    #[error("protected: {0}")]
    Protected(String),
    #[error("credential conflict: {0}")]
    CredentialConflict(String),
    #[error("password hashing error: {0}")]
    PasswordHash(String),
}

pub struct Store {
    data_dir: PathBuf,
    app_path: PathBuf,
    pub(crate) app: Connection,
    pub(crate) catalog: Connection,
    pub(crate) library: Connection,
    pub(crate) subscribe: Connection,
}
