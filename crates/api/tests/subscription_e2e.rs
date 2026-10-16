//! End-to-end subscription/download flow tests (see docs/subscription-fix-plan.md).
//! Run: `cargo test -p api --test subscription_e2e`
#![allow(dead_code)]

#[path = "management/common.rs"]
mod common;
#[path = "subscription_e2e/harness.rs"]
mod harness;
#[path = "subscription_e2e/mock_qbit.rs"]
mod mock_qbit;
#[path = "subscription_e2e/mock_transmission.rs"]
mod mock_transmission;

#[path = "subscription_e2e/fake_catalog.rs"]
mod fake_catalog;

#[path = "subscription_e2e/d_h1_qbit_relogin.rs"]
mod d_h1_qbit_relogin;
#[path = "subscription_e2e/d_h2_existing_infohash.rs"]
mod d_h2_existing_infohash;
#[path = "subscription_e2e/d_h3_m8_no_overwrite.rs"]
mod d_h3_m8_no_overwrite;
#[path = "subscription_e2e/d_m4_torrent_validation.rs"]
mod d_m4_torrent_validation;
#[path = "subscription_e2e/d_m5_manual_submit.rs"]
mod d_m5_manual_submit;
#[path = "subscription_e2e/d_m3_persisted_infohash.rs"]
mod d_m3_persisted_infohash;
#[path = "subscription_e2e/d_s1_pending_cleanup.rs"]
mod d_s1_pending_cleanup;
#[path = "subscription_e2e/g_h1_air_date_gate.rs"]
mod g_h1_air_date_gate;
#[path = "subscription_e2e/g_h2_season_inferred.rs"]
mod g_h2_season_inferred;
#[path = "subscription_e2e/g_m2_existing_task_state.rs"]
mod g_m2_existing_task_state;
#[path = "subscription_e2e/g_m5_instance_lock.rs"]
mod g_m5_instance_lock;
#[path = "subscription_e2e/golden_path.rs"]
mod golden_path;
#[path = "subscription_e2e/h1_wash_in_flight.rs"]
mod h1_wash_in_flight;
#[path = "subscription_e2e/h2_probe_rejected.rs"]
mod h2_probe_rejected;
#[path = "subscription_e2e/h3_background_reconcile.rs"]
mod h3_background_reconcile;
#[path = "subscription_e2e/h3_stalled_timeout.rs"]
mod h3_stalled_timeout;
#[path = "subscription_e2e/h4_season_pack_pending.rs"]
mod h4_season_pack_pending;
#[path = "subscription_e2e/g_h2b_absolute_episode.rs"]
mod g_h2b_absolute_episode;
#[path = "subscription_e2e/g_m1_overlapping_subscriptions.rs"]
mod g_m1_overlapping_subscriptions;
#[path = "subscription_e2e/g_m3_auto_next_season.rs"]
mod g_m3_auto_next_season;
#[path = "subscription_e2e/g_m4_pack_fallback.rs"]
mod g_m4_pack_fallback;
#[path = "subscription_e2e/l1_small_season_pack.rs"]
mod l1_small_season_pack;
#[path = "subscription_e2e/l2_wider_pack.rs"]
mod l2_wider_pack;
#[path = "subscription_e2e/l4_completion_marker.rs"]
mod l4_completion_marker;
#[path = "subscription_e2e/l5_rejected_episode_bound.rs"]
mod l5_rejected_episode_bound;
#[path = "subscription_e2e/l6_deletion_lock_audit.rs"]
mod l6_deletion_lock_audit;
#[path = "subscription_e2e/d_l1_partial_collect.rs"]
mod d_l1_partial_collect;
#[path = "subscription_e2e/d_l2_filtered_info.rs"]
mod d_l2_filtered_info;
#[path = "subscription_e2e/d_l3_loose_match_audit.rs"]
mod d_l3_loose_match_audit;
#[path = "subscription_e2e/g_l1_library_scoped_completion.rs"]
mod g_l1_library_scoped_completion;
#[path = "subscription_e2e/h2_selective_inferred_season.rs"]
mod h2_selective_inferred_season;
#[path = "subscription_e2e/h3_claimed_task.rs"]
mod h3_claimed_task;
#[path = "subscription_e2e/m1_m2_absent_evidence.rs"]
mod m1_m2_absent_evidence;
#[path = "subscription_e2e/m4_probe_veto_state.rs"]
mod m4_probe_veto_state;
#[path = "subscription_e2e/n1_probe_before_publish.rs"]
mod n1_probe_before_publish;
#[path = "subscription_e2e/m1_overlap_policy.rs"]
mod m1_overlap_policy;
#[path = "subscription_e2e/m2_owned_slot_skip.rs"]
mod m2_owned_slot_skip;
#[path = "subscription_e2e/m3_split_admission.rs"]
mod m3_split_admission;
#[path = "subscription_e2e/f1_selective_files.rs"]
mod f1_selective_files;
#[path = "subscription_e2e/m4a_completed_state.rs"]
mod m4a_completed_state;
#[path = "subscription_e2e/m5_follow_future.rs"]
mod m5_follow_future;
#[path = "subscription_e2e/m7_abandoned_transfer.rs"]
mod m7_abandoned_transfer;
#[path = "subscription_e2e/m6_deletion_retry.rs"]
mod m6_deletion_retry;
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
