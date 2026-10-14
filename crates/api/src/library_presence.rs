//! Library file reading trait and presence reconciliation.
//!
//! Provides `LibraryFileRead` abstraction (production and mock/fake)
//! and orchestrates verify and prune operations across bounded blocking work,
//! lease publication barrier, Store checks, and probe cancellation.

use std::path::Path;
use std::sync::Arc;

use domain::MediaId;
use store::{
    FilePresence, LedgerPresenceMode, LedgerPresenceObservation, LedgerPresenceResult,
    LibraryPathScope, StoreError, StrictOwner,
};

use crate::management::ApiState;

pub trait LibraryFileRead: Send + Sync {
    /// Minimum health check for a library root: exists, is a directory, readable.
    fn readable_root(&self, path: &Path) -> std::io::Result<()>;
    /// Check existence of a single file. Only explicit NotFound converts to Ok(false).
    fn presence(&self, path: &Path) -> std::io::Result<bool>;
}

#[derive(Default)]
pub struct SystemLibraryFileRead;

impl LibraryFileRead for SystemLibraryFileRead {
    fn readable_root(&self, path: &Path) -> std::io::Result<()> {
        let meta = std::fs::metadata(path)?;
        if !meta.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "root is not a directory",
            ));
        }
        // Try opening directory read
        let mut reader = std::fs::read_dir(path)?;
        // Fetch first entry or end of empty dir
        if let Some(res) = reader.next() {
            res?;
        }
        Ok(())
    }

    fn presence(&self, path: &Path) -> std::io::Result<bool> {
        match std::fs::metadata(path) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
}

#[derive(Debug)]
pub enum PresenceError {
    Missing,
    Deleting,
    Ambiguous,
    RootUnavailable,
    ScopeChanged,
    Busy,
    Store(StoreError),
}

impl From<StoreError> for PresenceError {
    fn from(err: StoreError) -> Self {
        Self::Store(err)
    }
}

impl std::fmt::Display for PresenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => f.write_str("library not found"),
            Self::Deleting => f.write_str("library is deleting"),
            Self::Ambiguous => f.write_str("library ownership is ambiguous"),
            Self::RootUnavailable => f.write_str("library root is unavailable"),
            Self::ScopeChanged => f.write_str("library scope changed"),
            Self::Busy => f.write_str("system is currently busy"),
            Self::Store(error) => write!(f, "store: {error}"),
        }
    }
}

/// Failure inside the presence publication barrier, typed **at the source that
/// produces it**.
///
/// Lifecycle races are classified by variant; the rendered text exists for logs
/// and user-visible messages only and must never drive classification.
#[derive(Debug)]
pub(crate) enum PresencePublishError {
    /// The Library is frozen for removal (or already gone).
    LibraryDeleting,
    /// The captured scope no longer matches the persisted ownership snapshot.
    ScopeChanged,
    /// Cooperative cancellation of the Library work lease.
    Cancelled,
    /// Store failure while applying the presence transaction.
    Store(StoreError),
}

impl From<StoreError> for PresencePublishError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl std::fmt::Display for PresencePublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 用户可见文案保持原样：发布闭包过去直接返回这些字符串。
        match self {
            Self::LibraryDeleting => f.write_str("library is deleting"),
            Self::ScopeChanged => f.write_str("library scope changed"),
            Self::Cancelled => f.write_str("Library work cancelled"),
            Self::Store(error) => write!(f, "store: {error}"),
        }
    }
}

/// Map a typed barrier failure onto the caller-facing [`PresenceError`].
///
/// Deliberately never inspects the rendered message: a store failure whose text
/// happens to contain `"library is deleting"` stays a store failure.
fn classify_publish_failure(error: PresencePublishError) -> PresenceError {
    match error {
        PresencePublishError::LibraryDeleting => PresenceError::Deleting,
        // 租约被取消同样意味着该 Library 已冻结/正在删除。
        PresencePublishError::Cancelled => PresenceError::Deleting,
        PresencePublishError::ScopeChanged => PresenceError::ScopeChanged,
        PresencePublishError::Store(error) => PresenceError::Store(error),
    }
}

/// Run the publication barrier and classify its failure by type.
///
/// `LibraryWorkLease::publish` (and the `marker::WorkControl` under it) only
/// speak `String`, and they can reject *before* the closure runs. The closure's
/// typed error is therefore captured on the side, and a lease-level rejection is
/// recognised from the lease's own cancellation flag — never by matching text.
pub(crate) fn publish_presence<T>(
    lease: &crate::library_lifecycle::LibraryWorkLease,
    publish: impl FnOnce() -> Result<T, PresencePublishError>,
) -> Result<T, PresenceError> {
    let mut typed: Option<PresencePublishError> = None;
    let outcome = lease.publish(|| match publish() {
        Ok(value) => Ok(value),
        Err(error) => {
            let text = error.to_string();
            typed = Some(error);
            Err(text)
        }
    });
    outcome.map_err(|text| match typed {
        Some(error) => classify_publish_failure(error),
        // 闭包没跑：租约包装层的取消没有类型可用，读它自己的取消标志。
        None if lease.control().is_cancelled() => {
            classify_publish_failure(PresencePublishError::Cancelled)
        }
        // 既没跑闭包、又没有取消标志（例如发布闸门 poison）：真实故障，保持 500。
        None => PresenceError::Store(StoreError::Invalid(text)),
    })
}

pub(crate) fn verify(
    state: &ApiState,
    library_id: &str,
) -> Result<LedgerPresenceResult, PresenceError> {
    reconcile_presence(state, library_id, None, LedgerPresenceMode::Verify)
}

pub(crate) fn prune(
    state: &ApiState,
    library_id: &str,
    media_id: Option<MediaId>,
) -> Result<LedgerPresenceResult, PresenceError> {
    reconcile_presence(state, library_id, media_id, LedgerPresenceMode::Prune)
}

fn collect_presence_observations(
    state: &ApiState,
    library_id: &str,
    target_media_id: Option<MediaId>,
    candidate_scope: &LibraryPathScope,
    candidate_rows: Vec<domain::LedgerRow>,
    missing_map: &std::collections::HashMap<String, i64>,
    media_kinds: &std::collections::HashMap<MediaId, domain::MediaKind>,
) -> Result<Vec<LedgerPresenceObservation>, PresenceError> {
    let mut observations = Vec::new();

    for row in candidate_rows {
        if let Some(mid) = target_media_id {
            if row.media_id != mid {
                continue;
            }
        }

        let Some(&real_kind) = media_kinds.get(&row.media_id) else {
            // Missing media identity: skip conservatively
            continue;
        };

        let Ok(resolved) = LibraryPathScope::resolve_path(Path::new(&row.path)) else {
            observations.push(LedgerPresenceObservation {
                row: row.clone(),
                missing_at: missing_map.get(&row.path).copied(),
                presence: FilePresence::Unknown,
            });
            continue;
        };

        match candidate_scope.owner(&resolved, real_kind) {
            StrictOwner::Owned(owner) if owner.id == library_id => {
                // Eligible row
            }
            StrictOwner::Ambiguous => return Err(PresenceError::Ambiguous),
            _ => continue,
        }

        let presence = match state.library_files.presence(Path::new(&row.path)) {
            Ok(true) => FilePresence::Present,
            Ok(false) => FilePresence::Missing,
            Err(e) => {
                tracing::warn!(path = %row.path, error = %e, "Observing file presence failed");
                FilePresence::Unknown
            }
        };

        observations.push(LedgerPresenceObservation {
            row: row.clone(),
            missing_at: missing_map.get(&row.path).copied(),
            presence,
        });
    }

    Ok(observations)
}

fn reconcile_presence(
    state: &ApiState,
    library_id: &str,
    target_media_id: Option<MediaId>,
    mode: LedgerPresenceMode,
) -> Result<LedgerPresenceResult, PresenceError> {
    // 1. In Store lock, take Library configuration, removal state, and snapshot.
    let (target_lib, snapshot, missing_map) = {
        let guard = state.store.lock();
        let snapshot = guard.library_ownership()?;
        let lib = snapshot
            .library(library_id)
            .cloned()
            .ok_or(PresenceError::Missing)?;
        if !snapshot.accepts_work(library_id) {
            return Err(PresenceError::Deleting);
        }
        let missing_map = guard.missing_at_by_path()?;
        (lib, snapshot, missing_map)
    };

    // 2. Resolve scope out of lock
    let scope = LibraryPathScope::resolve(snapshot)?;
    if !scope.accepts_work(library_id) {
        return Err(PresenceError::Deleting);
    }

    // Check roots readability
    for root in &target_lib.root_paths {
        if let Err(e) = state.library_files.readable_root(root) {
            tracing::warn!(root = %root.display(), error = %e, "Library root unavailable");
            return Err(PresenceError::RootUnavailable);
        }
    }

    // Collect candidates for this library and batch load their media kinds
    let (candidate_rows, media_kinds, candidate_scope) = {
        let guard = state.store.lock();
        let rows = guard.list_ledger()?;
        let media_ids: Vec<MediaId> = rows.iter().map(|r| r.media_id).collect();
        let media_map = guard.media_batch(&media_ids)?;
        let kinds: std::collections::HashMap<MediaId, domain::MediaKind> = media_map
            .into_iter()
            .map(|(mid, m)| (mid, m.kind))
            .collect();
        (rows, kinds, scope.clone())
    };

    let mut observations = collect_presence_observations(
        state,
        library_id,
        target_media_id,
        &candidate_scope,
        candidate_rows,
        &missing_map,
        &media_kinds,
    )?;

    // If in Prune mode: verify presence one last time and cancel probe work
    if mode == LedgerPresenceMode::Prune {
        for obs in &mut observations {
            if obs.presence == FilePresence::Missing && obs.missing_at.is_some() {
                // Re-verify immediately before pruning
                match state.library_files.presence(Path::new(&obs.row.path)) {
                    Ok(true) => obs.presence = FilePresence::Present,
                    Ok(false) => obs.presence = FilePresence::Missing,
                    Err(e) => {
                        tracing::warn!(path = %obs.row.path, error = %e, "Re-verifying file presence returned error, skipping deletion");
                        obs.presence = FilePresence::Unknown;
                    }
                }
            }
        }

        let ids_to_cancel: Vec<String> = observations
            .iter()
            .filter(|obs| obs.presence == FilePresence::Missing && obs.missing_at.is_some())
            .map(|obs| obs.row.id.to_string())
            .collect();
        if !ids_to_cancel.is_empty() {
            state
                .probe
                .cancel_ledger_work(&ids_to_cancel, "pruning missing files")?;
        }
    }

    // 3. Acquire lease: Registry -> Store
    let lease = match state.acquire_library_work(library_id) {
        Ok(lease) => lease,
        Err(_) => {
            return Err(PresenceError::Deleting);
        }
    };

    // 4. In publication barrier, check scope matches and apply presence in a single Immediate transaction.
    //    失败按 `PresencePublishError` 的类型分类，不再匹配错误文案。
    publish_presence(&lease, || {
        let guard = state.store.lock();
        let current_snapshot = guard.library_ownership()?;
        if !current_snapshot.accepts_work(library_id) {
            return Err(PresencePublishError::LibraryDeleting);
        }
        if !candidate_scope.matches(&current_snapshot) {
            return Err(PresencePublishError::ScopeChanged);
        }

        let now = crate::job_loop::unix_now();
        guard
            .apply_ledger_presence(&observations, mode, now)
            .map_err(PresencePublishError::from)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 分类只看类型：Store 故障即使文案里出现 `library is deleting`，也不能被
    /// 当成生命周期竞争 —— 旧实现正是 `err_str.contains("library is deleting")`。
    #[test]
    fn publish_failure_is_classified_by_variant_not_message() {
        let disguised =
            PresencePublishError::Store(StoreError::Invalid("library is deleting".to_string()));
        assert!(matches!(
            classify_publish_failure(disguised),
            PresenceError::Store(_)
        ));
        assert!(matches!(
            classify_publish_failure(PresencePublishError::ScopeChanged),
            PresenceError::ScopeChanged
        ));
        assert!(matches!(
            classify_publish_failure(PresencePublishError::LibraryDeleting),
            PresenceError::Deleting
        ));
        // 租约取消 = Library 已冻结/正在删除。
        assert!(matches!(
            classify_publish_failure(PresencePublishError::Cancelled),
            PresenceError::Deleting
        ));
    }

    /// 发布闭包看到的文案保持原样（用户可见消息不变）。
    #[test]
    fn publish_failure_messages_are_unchanged() {
        assert_eq!(
            PresencePublishError::LibraryDeleting.to_string(),
            "library is deleting"
        );
        assert_eq!(
            PresencePublishError::ScopeChanged.to_string(),
            "library scope changed"
        );
        assert_eq!(
            PresencePublishError::Cancelled.to_string(),
            "Library work cancelled"
        );
        assert!(
            PresencePublishError::Store(StoreError::Invalid("boom".into()))
                .to_string()
                .starts_with("store: ")
        );
    }
}
