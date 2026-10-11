use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use marker::adaptive::{
    EpisodeEvidence, SamplingPolicy, SourceCostSummary, TemplateContext, VerifiedInterval,
};
use marker::fingerprint::capture_types::FingerprintCaptureEngine;
use marker::FingerprintEngine;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use store::Store;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturePolicy {
    ReuseValid,
    Recapture,
}

pub trait CaptureGate: Send + Sync {
    fn work_control(&self, _row: &domain::LedgerRow) -> marker::WorkControl {
        marker::WorkControl::default()
    }
    fn wait_before_capture(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

pub struct AdaptiveCaptureContext {
    pub store: Arc<Mutex<Store>>,
    pub matcher: Arc<dyn FingerprintEngine>,
    pub capture: Arc<dyn FingerprintCaptureEngine>,
    pub gate: Arc<dyn CaptureGate>,
    pub timings: Option<crate::probe_manager::timings::ProbeTimingLedger>,
}

#[derive(Clone, Debug)]
pub struct EpisodeCaptureRequest {
    pub job_id: String,
    pub row: domain::LedgerRow,
    pub source_version: String,
    pub media_duration_ms: Option<i64>,
    pub audio_stream_index: Option<u32>,
    pub capture_profile_key: String,
    pub policy: SamplingPolicy,
    pub capture_policy: CapturePolicy,
    pub templates: TemplateContext,
    pub cost_summary: SourceCostSummary,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EpisodeDetection {
    pub ledger_id: String,
    pub episode: u32,
    pub intro_evidence: Option<EpisodeEvidence>,
    pub outro_evidence: Option<EpisodeEvidence>,
    pub intro_match: Option<VerifiedInterval>,
    pub outro_match: Option<VerifiedInterval>,
    #[serde(default)]
    pub intro_outcome: Option<marker::adaptive::VerificationOutcome>,
    #[serde(default)]
    pub outro_outcome: Option<marker::adaptive::VerificationOutcome>,
}
