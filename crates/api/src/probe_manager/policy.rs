use std::path::Path;
use domain::MediaKind;
use crate::scrape_store::ScrapeStoreExt;
use store::Store;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeRequestOrigin {
    Filesystem,
    Detail,
    Playback,
    BackgroundRetry,
    ManualRetry,
    ManualRefresh,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeRequestResult {
    Complete,
    Queued { job_id: String },
    AlreadyRunning,
    Waiting { next_retry_at_ms: i64 },
    Exhausted,
    Disabled,
    Cancelled,
}

/// Evaluates whether a library allows automatic intro/outro detection and fingerprinting.
pub fn is_fingerprint_enabled_for_path(store: &Store, path: &Path, kind: MediaKind) -> bool {
    if kind != MediaKind::Tv {
        return false;
    }
    matching_library(store, path, kind)
        .is_some_and(|library| library.detect_intros && library.enable_fingerprint)
}

/// Evaluates whether marker detection (the master switch) is enabled.
pub fn is_marker_detection_enabled_for_path(store: &Store, path: &Path, kind: MediaKind) -> bool {
    if kind != MediaKind::Tv {
        return false;
    }
    matching_library(store, path, kind).is_none_or(|library| library.detect_intros)
}

pub(super) struct ProbeNeed {
    pub kind: MediaKind,
    pub fingerprint_enabled: bool,
    pub metadata_valid: bool,
    pub intro_missing: bool,
    pub outro_missing: bool,
    pub media_duration_ms: Option<i64>,
    pub source_version: String,
    pub sample_duration_secs: u32,
    pub retry: ProbeRequestResult,
}

pub(super) fn assess_probe_need(
    store: &Store,
    row: &domain::LedgerRow,
    origin: ProbeRequestOrigin,
    now_ms: i64,
    source_version: String,
) -> ProbeNeed {
    let ledger_id = row.id.to_string();
    let kind = store
        .get_media(row.media_id)
        .ok()
        .flatten()
        .map(|media| media.kind)
        .unwrap_or(MediaKind::Movie);
    let path = Path::new(&row.path);
    let fingerprint_enabled = is_fingerprint_enabled_for_path(store, path, kind);
    let sample_duration_secs = store
        .get_scrape_config()
        .ok()
        .map(|config| config.effective.fingerprint_duration_secs)
        .unwrap_or(180);
    let cached_meta = store.get_media_info_cache_version(&ledger_id).ok().flatten();
    let metadata_valid = cached_meta.as_ref().is_some_and(|version| {
        version.source_version == source_version
            && version.format_duration_ms.is_some_and(|duration| duration > 0)
    });
    let media_duration_ms = cached_meta.and_then(|version| version.format_duration_ms);
    let outro_expected = media_duration_ms.filter(|duration| *duration > 0).is_some_and(
        |duration| duration / 1000 > i64::from(sample_duration_secs.saturating_add(30)),
    );
    let expected_key = crate::fingerprint_job::fingerprint_cache_key(
        &source_version,
        sample_duration_secs,
        media_duration_ms,
    );
    let cached = store.get_fingerprint_cache(&ledger_id).ok().flatten();
    let intro_missing = fingerprint_enabled
        && cached
            .as_ref()
            .is_none_or(|entry| entry.cache_key != expected_key || entry.intro.is_empty());
    let outro_missing = fingerprint_enabled
        && outro_expected
        && cached.as_ref().is_none_or(|entry| {
            entry.cache_key != expected_key || entry.outro.as_ref().is_none_or(Vec::is_empty)
        });
    let retry = retry_gate(
        store,
        origin,
        &ledger_id,
        &expected_key,
        fingerprint_enabled,
        intro_missing,
        outro_missing,
        now_ms,
    );
    ProbeNeed {
        kind,
        fingerprint_enabled,
        metadata_valid,
        intro_missing,
        outro_missing,
        media_duration_ms,
        source_version,
        sample_duration_secs,
        retry,
    }
}

fn retry_gate(
    store: &Store,
    origin: ProbeRequestOrigin,
    ledger_id: &str,
    context_key: &str,
    fingerprint_enabled: bool,
    intro_missing: bool,
    outro_missing: bool,
    now_ms: i64,
) -> ProbeRequestResult {
    if store
        .active_probe_unit_for_ledger(ledger_id)
        .ok()
        .flatten()
        .is_some()
    {
        return ProbeRequestResult::AlreadyRunning;
    }
    let manual = matches!(
        origin,
        ProbeRequestOrigin::ManualRetry | ProbeRequestOrigin::ManualRefresh
    );
    if manual || !fingerprint_enabled || !outro_missing || intro_missing {
        return ProbeRequestResult::Complete;
    }
    let key = store::ProbeStageKey {
        ledger_id: ledger_id.to_string(),
        context_key: context_key.to_string(),
        stage: store::ProbeStage::Outro,
    };
    let Ok(Some(stage)) = store.get_probe_stage(&key) else {
        return ProbeRequestResult::Complete;
    };
    if stage.status != store::ProbeStageStatus::Failed {
        return ProbeRequestResult::Complete;
    }
    if stage.failure_count >= 5 {
        return ProbeRequestResult::Exhausted;
    }
    match stage.next_retry_at_ms {
        Some(next_retry_at_ms) if next_retry_at_ms > now_ms => {
            ProbeRequestResult::Waiting { next_retry_at_ms }
        }
        _ => ProbeRequestResult::Complete,
    }
}

fn matching_library(store: &Store, path: &Path, kind: MediaKind) -> Option<store::Library> {
    store.library_for_path(path, kind).ok().flatten().or_else(|| {
        store.list_libraries().ok().and_then(|libraries| {
            libraries.into_iter().find(|library| {
                library.kind == kind
                    && library
                        .root_paths
                        .iter()
                        .any(|root| path.starts_with(root))
            })
        })
    })
}
