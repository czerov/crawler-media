pub mod capture;
pub mod capture_types;
mod chromaprint;
mod segments;

pub use capture_types::{
    CaptureFailure, CaptureFailureKind, CaptureMetrics, CaptureRequest, CapturedFingerprint,
    FingerprintCaptureEngine, InputBytesSource, SampleWindow,
};
pub use chromaprint::diagnostics::sanitized_excerpt as sanitize_stderr;

use std::path::Path;
use std::time::Instant;

use crate::target::ProbeTarget;
use crate::types::CommonSegment;

pub type AudioFingerprint = Vec<u32>;

pub const MIN_MATCH_DURATION_SECS: f32 = 15.0;
pub const MAX_MATCH_DURATION_SECS: f32 = 240.0;
const MAX_REMOTE_SAMPLE_ATTEMPTS: u8 = 4;
const REMOTE_SAMPLE_RETRY_INITIAL_DELAY_MS: u64 = 500;

/// Replaceable boundary for audio fingerprint extraction and matching.
pub trait FingerprintEngine: Send + Sync {
    fn extract_at(
        &self,
        path: &Path,
        start_secs: u32,
        duration_secs: u32,
    ) -> Result<AudioFingerprint, String>;

    fn find_common_segment(
        &self,
        first: &[u32],
        second: &[u32],
        min_duration_secs: f32,
        max_duration_secs: f32,
    ) -> Option<CommonSegment>;

    /// Return every distinct matching interval so season consensus can prefer
    /// repeated episode-wide evidence over one long accidental pair match.
    fn find_common_segments(
        &self,
        first: &[u32],
        second: &[u32],
        min_duration_secs: f32,
        max_duration_secs: f32,
    ) -> Vec<CommonSegment> {
        self.find_common_segment(first, second, min_duration_secs, max_duration_secs)
            .into_iter()
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ChromaprintEngine;

impl FingerprintCaptureEngine for ChromaprintEngine {
    fn capture_window_controlled(
        &self,
        request: &CaptureRequest,
        control: &crate::WorkControl,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        capture::capture_window_controlled(request, control)
    }
    fn capture_window(
        &self,
        request: &CaptureRequest,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        capture::capture_window_chromaprint(request)
    }
}

impl FingerprintEngine for ChromaprintEngine {
    fn extract_at(
        &self,
        path: &Path,
        start_secs: u32,
        duration_secs: u32,
    ) -> Result<AudioFingerprint, String> {
        chromaprint::extract(path, start_secs, duration_secs)
    }

    fn find_common_segment(
        &self,
        first: &[u32],
        second: &[u32],
        min_duration_secs: f32,
        max_duration_secs: f32,
    ) -> Option<CommonSegment> {
        segments::find_common_segment(first, second, min_duration_secs, max_duration_secs)
    }

    fn find_common_segments(
        &self,
        first: &[u32],
        second: &[u32],
        min_duration_secs: f32,
        max_duration_secs: f32,
    ) -> Vec<CommonSegment> {
        segments::find_common_segments(first, second, min_duration_secs, max_duration_secs)
    }
}

/// Increment when the fingerprint sampling or Chromaprint profile changes.
pub const FINGERPRINT_ALGORITHM_VERSION: u32 = 1;

pub fn extract_audio_fingerprint(path: &Path, max_duration_secs: u32) -> Result<Vec<u32>, String> {
    extract_audio_fingerprint_at(path, 0, max_duration_secs)
}

pub fn extract_audio_fingerprint_at(
    path: &Path,
    start_secs: u32,
    duration_secs: u32,
) -> Result<Vec<u32>, String> {
    extract_audio_fingerprint_at_with(&ChromaprintEngine, path, start_secs, duration_secs)
}

pub fn extract_audio_fingerprint_at_with(
    engine: &dyn FingerprintEngine,
    path: &Path,
    start_secs: u32,
    duration_secs: u32,
) -> Result<AudioFingerprint, String> {
    let source = match ProbeTarget::from_path(path) {
        ProbeTarget::Local(_) => "local",
        ProbeTarget::Remote(_) => "remote",
    };
    let started = Instant::now();
    tracing::info!(
        path = %path.display(), source, start_secs, duration_secs,
        "【声纹】音频指纹生成开始"
    );

    let mut attempt = 1;
    let result = loop {
        let attempt_started = Instant::now();
        let result = engine.extract_at(path, start_secs, duration_secs);
        let retryable_read_error =
            matches!(&result, Err(error) if is_retryable_remote_read_error(error));
        if source != "remote" || !retryable_read_error || attempt >= MAX_REMOTE_SAMPLE_ATTEMPTS {
            break result;
        }
        if let Err(error) = &result {
            let retry_delay_ms =
                REMOTE_SAMPLE_RETRY_INITIAL_DELAY_MS * 2u64.pow(u32::from(attempt - 1));
            tracing::warn!(
                path = %path.display(),
                source,
                attempt,
                next_attempt = attempt + 1,
                max_attempts = MAX_REMOTE_SAMPLE_ATTEMPTS,
                attempt_elapsed_ms = attempt_started.elapsed().as_millis(),
                retry_delay_ms,
                error = %error,
                "【声纹】远端音频样本读取异常，等待后重试"
            );
            std::thread::sleep(std::time::Duration::from_millis(retry_delay_ms));
        }
        attempt += 1;
    };
    let elapsed_ms = started.elapsed().as_millis();
    match &result {
        Ok(fingerprint) => tracing::info!(
            path = %path.display(), source, start_secs, duration_secs,
            attempts = attempt, fingerprint_items = fingerprint.len(), elapsed_ms,
            "【声纹】音频指纹生成成功"
        ),
        Err(error) => tracing::error!(
            path = %path.display(), source, start_secs, duration_secs,
            attempts = attempt, elapsed_ms, error = %error,
            "【声纹】音频指纹生成失败"
        ),
    }
    result
}

fn is_retryable_remote_read_error(error: &str) -> bool {
    error.contains("File ended prematurely") || error.contains("Read error")
}

pub fn find_common_segment(
    first: &[u32],
    second: &[u32],
    min_duration_secs: f32,
    max_duration_secs: f32,
) -> Option<CommonSegment> {
    find_common_segment_with(
        &ChromaprintEngine,
        first,
        second,
        min_duration_secs,
        max_duration_secs,
    )
}

pub fn find_common_segment_with(
    engine: &dyn FingerprintEngine,
    first: &[u32],
    second: &[u32],
    min_duration_secs: f32,
    max_duration_secs: f32,
) -> Option<CommonSegment> {
    engine.find_common_segment(first, second, min_duration_secs, max_duration_secs)
}
