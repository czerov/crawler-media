//! End-to-end subscription/download flow tests (see docs/subscription-fix-plan.md).
//! Run: `cargo test -p api --test subscription_e2e`
#![allow(dead_code)]

#[path = "management/common.rs"]
mod common;
#[path = "subscription_e2e/harness.rs"]
mod harness;
#[path = "subscription_e2e/mock_qbit.rs"]
mod mock_qbit;

#[path = "subscription_e2e/golden_path.rs"]
mod golden_path;
#[path = "subscription_e2e/d_s1_pending_cleanup.rs"]
mod d_s1_pending_cleanup;
#[path = "subscription_e2e/s1_wash_probe_fail.rs"]
mod s1_wash_probe_fail;

pub(crate) fn is_video(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("mkv" | "mp4" | "avi" | "ts" | "m2ts")
    )
}
