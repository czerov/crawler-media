//! System-of-record store for crawler-media (ADR-0008).
//! Backed by 4 SQLite files in the data directory.

mod app_write;
mod instance_lock;
mod pending_file_deletions;
mod pending_progress;
mod browse_projection;
pub use browse_projection::{
    BrowseFileFacts, BrowseMediaFacts, ProjectionBuild, ProjectionSnapshot, ReadRevisions,
    library_scope_signature,
};
mod browse_changes;
pub use browse_changes::{BrowseChange, BrowseChangeSource, ProjectionHead};
mod browse_pages;
pub use browse_pages::{
    BROWSE_CANDIDATE_VERSION, BrowseFileDependency, BrowseMediaCandidate, BrowsePageOrder,
    BrowsePageRequest,
};
mod browse_reads;
mod catalog;
mod chapter_frame_tasks;
pub use chapter_frame_tasks::{ChapterFrameTask, ChapterFrameTaskCounts};
mod collections;
mod data_retention;
mod downloaders;
mod fingerprint_attempts;
mod fingerprint_cache;
mod fingerprint_models;
mod fingerprint_samples;
mod ledger;
mod ledger_cleanup;
mod ledger_quality;
mod ledger_presence;
pub use ledger_presence::{
    FilePresence, LedgerPresenceMode, LedgerPresenceObservation, LedgerPresenceResult,
};
mod legacy_recycle;
mod libraries;
mod library;
mod library_covers;
mod library_deletion;
pub use library_deletion::LibraryRemoval;
pub mod library_defaults;
mod library_ownership;
mod library_paths;
mod library_scope;
pub use library_scope::{LibraryPathResolver, LibraryPathScope, StrictOwner};
mod subscribe_library_snapshot;
pub use subscribe_library_snapshot::SubscribeLibrarySnapshot;
pub use pending_file_deletions::{MAX_FILE_DELETION_ATTEMPTS, QueuedFileDeletion, file_identity};
mod subscribe_library;
pub use subscribe_library::evaluate_subscribe_facts_pure;
mod maps;
mod media;
mod open;
mod playback;
mod playback_devices;
mod probe_tasks;
mod probe_dispatch;
pub use probe_dispatch::{ProbeDispatchId, ProbeDispatchSnapshot, ProbeDispatchStage};
mod roots;
mod schema;
mod scanned_ledger;
mod scan_probe_obligations;
mod scan_operations;
mod scan_seen;
pub use scan_operations::{ScanOperation, ScanOperationStatus};
pub use scan_probe_obligations::ScanProbeObligation;
mod search_history;
pub use search_history::{HistoryDeleteOutcome, HistoryDeletePlan};
mod sites;
mod subscribe_wanted;
mod subscribes;
mod users;

pub mod password;
pub mod probe_state;

pub use browse_reads::{BrowseFileTotals, PlaybackUnitSnapshot};
pub use catalog::CatalogCacheRow;
pub use data_retention::{DataRetentionBatch, DataRetentionCursor, RetentionClass};
pub use downloaders::DownloaderRow;
pub use fingerprint_attempts::StoredFingerprintAttempt;
pub use fingerprint_cache::{FingerprintCacheEntry, MediaInfoCacheVersion};
pub use fingerprint_models::{StoredFingerprintModel, StoredFingerprintModelMember};
pub use fingerprint_samples::{FingerprintSampleQuery, StoredFingerprintSample};
pub use libraries::Library;
pub use library::{MarkerResultReplacement, StoredMediaMarker};
pub use library_ownership::LibraryOwnership;
pub use playback::{
    PlayLogRow, PlaySessionBinding, PlaySessionIdState, PlaybackLogPage, PlaybackLogScope,
    PlaybackRetentionBatch, PlaybackRetentionPreview, SessionRow, UNIT_WHOLE, UnitRow, UnitState,
    WatchClient, WatchDay, WatchMedia, WatchMember, WatchReport, WatchTotals, WatchWindow,
};
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
    #[error("invalid argument: {0}")]
    Invalid(String),
    #[error("protected: {0}")]
    Protected(String),
    #[error("credential conflict: {0}")]
    CredentialConflict(String),
    #[error("password hashing error: {0}")]
    PasswordHash(String),
}

pub struct Store {
    data_dir: PathBuf,
    /// Times this store's `list_ledger` executed its unfiltered `FROM ledger`
    /// statement. Per-instance so concurrent tests never share the diagnostic.
    pub(crate) root_ledger_scans: std::sync::atomic::AtomicUsize,
    /// Calls to `ledger_for_media` on this store. Selection can load the same
    /// media twice; the logical candidate count does not show that second read.
    pub(crate) media_ledger_reads: std::sync::atomic::AtomicUsize,
    /// Per-store role lookup diagnostic for bounded roster work.
    pub(crate) role_lookups: std::sync::atomic::AtomicUsize,
    app_path: PathBuf,
    pub(crate) app: Connection,
    pub(crate) catalog: Connection,
    pub(crate) library: Connection,
    pub(crate) subscribe: Connection,
}

impl Store {
    /// Times `list_ledger` executed its unfiltered `FROM ledger` statement.
    ///
    /// Incremented beside that statement, so filtering the returned rows cannot
    /// hide the scan. `ledger_for_media` does not increment it.
    pub fn root_ledger_scans(&self) -> usize {
        self.root_ledger_scans.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn reset_root_ledger_scans(&self) {
        self.root_ledger_scans.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// Times `ledger_for_media` executed its indexed read.
    pub fn media_ledger_reads(&self) -> usize {
        self.media_ledger_reads.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn reset_media_ledger_reads(&self) {
        self.media_ledger_reads.store(0, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn role_lookups(&self) -> usize {
        self.role_lookups.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn reset_role_lookups(&self) {
        self.role_lookups.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}
