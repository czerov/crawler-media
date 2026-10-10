use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampleWindow {
    pub start_ms: i64,
    pub end_ms: i64,
}

impl SampleWindow {
    pub fn new(start_ms: i64, end_ms: i64) -> Self {
        Self { start_ms, end_ms }
    }

    pub fn duration_ms(&self) -> i64 {
        self.end_ms.saturating_sub(self.start_ms)
    }

    pub fn validate(&self) -> Result<(), CaptureFailure> {
        if self.start_ms < 0 || self.end_ms < 0 {
            return Err(CaptureFailure {
                kind: CaptureFailureKind::InvalidWindow,
                message: format!(
                    "Sample window cannot have negative timestamps: start_ms={}, end_ms={}",
                    self.start_ms, self.end_ms
                ),
                metrics: CaptureMetrics::default(),
            });
        }
        if self.start_ms >= self.end_ms {
            return Err(CaptureFailure {
                kind: CaptureFailureKind::InvalidWindow,
                message: format!(
                    "Sample window must have start_ms < end_ms: start_ms={}, end_ms={}",
                    self.start_ms, self.end_ms
                ),
                metrics: CaptureMetrics::default(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRequest {
    pub path: PathBuf,
    pub window: SampleWindow,
    pub audio_stream_index: Option<u32>,
    pub process_deadline_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputBytesSource {
    AvioInput,
    FixtureOrigin,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureMetrics {
    pub elapsed_ms: u64,
    pub time_to_first_pcm_ms: Option<u64>,
    pub pcm_read_wait_us: u64,
    pub chromaprint_consume_us: u64,
    pub pcm_bytes: u64,
    pub input_bytes: Option<u64>,
    pub input_bytes_source: Option<InputBytesSource>,
    pub measurement_complete: bool,
    pub ffmpeg_exit_code: Option<i32>,

    // Benchmark / CPU diagnostics extensions
    pub command_setup_ms: Option<u64>,
    pub ffmpeg_spawn_ms: Option<u64>,
    pub pcm_stream_elapsed_ms: Option<u64>,
    pub chromaprint_finish_ms: Option<u64>,
    pub ffmpeg_wait_ms: Option<u64>,
    pub stderr_collect_ms: Option<u64>,
    pub sample_count: Option<u64>,
    pub ffmpeg_user_cpu_ms: Option<u64>,
    pub ffmpeg_system_cpu_ms: Option<u64>,
    pub ffmpeg_real_ms: Option<u64>,
    pub ffmpeg_maxrss_kb: Option<u64>,
    pub ffmpeg_stderr_bytes: Option<u64>,
    pub ffmpeg_stderr_tail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedFingerprint {
    pub window: SampleWindow,
    pub words: Vec<u32>,
    pub pcm_duration_ms: Option<i64>,
    pub metrics: CaptureMetrics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureFailureKind {
    Io,
    Timeout,
    EmptyAudio,
    InvalidWindow,
    Decode,
    Cancelled,
}

#[derive(Clone, Debug, Error, PartialEq, Eq, Serialize, Deserialize)]
#[error("{kind:?}: {message}")]
pub struct CaptureFailure {
    pub kind: CaptureFailureKind,
    pub message: String,
    pub metrics: CaptureMetrics,
}

pub trait FingerprintCaptureEngine: Send + Sync {
    fn capture_window_controlled(
        &self,
        request: &CaptureRequest,
        control: &crate::WorkControl,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        if control.is_cancelled() {
            return Err(CaptureFailure { kind: CaptureFailureKind::Cancelled,
                message: "Library work cancelled".into(), metrics: CaptureMetrics::default() });
        }
        let result = self.capture_window(request);
        if control.is_cancelled() {
            return Err(CaptureFailure { kind: CaptureFailureKind::Cancelled,
                message: "Library work cancelled".into(), metrics: CaptureMetrics::default() });
        }
        result
    }
    fn capture_window(
        &self,
        request: &CaptureRequest,
    ) -> Result<CapturedFingerprint, CaptureFailure>;
}
