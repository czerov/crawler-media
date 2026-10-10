use std::path::PathBuf;
use std::time::SystemTime;

use marker::adaptive::{EpisodeEvidence, SegmentKind};
use marker::fingerprint::capture_types::{
    CaptureFailureKind, CaptureRequest, CapturedFingerprint, SampleWindow,
};
use store::{StoredFingerprintAttempt, StoredFingerprintSample};
use uuid::Uuid;

use super::cache::{find_reusable_sample, stored_sample_to_evidence};
use super::types::{AdaptiveCaptureContext, EpisodeCaptureRequest};
use crate::fingerprint_job::source::{ensure_source_available, is_file_deleted};

pub async fn capture_or_reuse_segment_sample(
    ctx: &AdaptiveCaptureContext,
    request: &EpisodeCaptureRequest,
    kind: SegmentKind,
    window: &SampleWindow,
    phase: &str,
    attempt_count: &mut usize,
    max_budget: usize,
) -> Result<EpisodeEvidence, String> {
    {
        let store = ctx.store.lock();
        ensure_source_available(&store, &request.row)?;
        if let Some(cached) = find_reusable_sample(&store, request, kind, window) {
            if let Some(timings) = &ctx.timings {
                match kind {
                    SegmentKind::Intro => timings.record_intro_cache_hit(Some(&request.job_id)),
                    SegmentKind::Outro => timings.record_outro_cache_hit(Some(&request.job_id)),
                }
            }
            tracing::info!(job_id = %request.job_id, ledger_id = %request.row.id, ?kind,
                "复用有效声纹缓存");
            return Ok(cached);
        }
    }
    if *attempt_count >= max_budget {
        return Err(format!(
            "Attempt budget exhausted ({}/{max_budget})",
            attempt_count
        ));
    }
    *attempt_count += 1;
    let mut attempt = begin_attempt(ctx, request, kind, window, phase)?;
    ctx.gate.wait_before_capture().await;
    let availability = ensure_source_available(&ctx.store.lock(), &request.row);
    if let Err(error) = availability {
        return finish_error(ctx, request, &mut attempt, error);
    }
    let capture_req = CaptureRequest {
        path: PathBuf::from(&request.row.path),
        window: window.clone(),
        audio_stream_index: request.audio_stream_index,
        process_deadline_ms: request.policy.process_deadline_ms,
    };
    let capture = ctx.capture.clone();
    let control = ctx.gate.work_control(&request.row);
    let result = tokio::task::spawn_blocking(move || capture.capture_window_controlled(&capture_req, &control)).await;
    // Recheck even when FFmpeg returned an error: a deletion is cancellation, not a retry.
    let availability = ensure_source_available(&ctx.store.lock(), &request.row);
    if let Err(error) = availability {
        return finish_error(ctx, request, &mut attempt, error);
    }
    match result {
        Ok(Ok(captured)) => finish_capture(ctx, request, kind, attempt, captured),
        Ok(Err(error)) => {
            attempt.error_kind = Some(capture_error_kind(&error.kind).into());
            attempt.metrics_json = serde_json::to_string(&error.metrics).unwrap_or_default();
            finish_error(
                ctx,
                request,
                &mut attempt,
                format!("Capture failed: {error}"),
            )
        }
        Err(error) => finish_error(
            ctx,
            request,
            &mut attempt,
            format!("Capture task failed: {error}"),
        ),
    }
}

fn begin_attempt(
    ctx: &AdaptiveCaptureContext,
    request: &EpisodeCaptureRequest,
    kind: SegmentKind,
    window: &SampleWindow,
    phase: &str,
) -> Result<StoredFingerprintAttempt, String> {
    let attempt = StoredFingerprintAttempt {
        attempt_id: format!("att-{}", Uuid::new_v4()),
        job_id: request.job_id.clone(),
        ledger_id: request.row.id.to_string(),
        kind: kind_name(kind).into(),
        window_start_ms: window.start_ms,
        window_end_ms: window.end_ms,
        phase: phase.into(),
        started_at_ms: now_ms(),
        finished_at_ms: None,
        status: "running".into(),
        error_kind: None,
        metrics_json: "{}".into(),
    };
    ctx.store
        .lock()
        .begin_fingerprint_attempt(&attempt)
        .map_err(|error| {
            tracing::error!(%error, job_id = %request.job_id, "声纹采集开始状态写入失败");
            format!("Store error beginning attempt: {error}")
        })?;
    Ok(attempt)
}

fn finish_error(
    ctx: &AdaptiveCaptureContext,
    request: &EpisodeCaptureRequest,
    attempt: &mut StoredFingerprintAttempt,
    error: String,
) -> Result<EpisodeEvidence, String> {
    attempt.finished_at_ms = Some(now_ms());
    let cancelled = is_file_deleted(&error);
    attempt.status = if cancelled { "cancelled" } else { "failed" }.into();
    if cancelled {
        attempt.error_kind = Some("file_deleted".into());
    }
    let elapsed_ms = now_ms().saturating_sub(attempt.started_at_ms);
    if let Err(store_error) = ctx.store.lock().complete_fingerprint_attempt(attempt, None) {
        tracing::error!(%store_error, job_id = %request.job_id, "声纹采集结束状态写入失败");
    }
    if cancelled {
        tracing::info!(job_id = %request.job_id, ledger_id = %request.row.id,
            path = %request.row.path, stage = %attempt.phase, elapsed_ms, reason = "file_deleted",
            "文件已删除，取消声纹采集");
    } else {
        tracing::error!(%error, job_id = %request.job_id, ledger_id = %request.row.id,
            path = %request.row.path, stage = %attempt.phase, elapsed_ms, "声纹采集失败");
    }
    Err(error)
}

fn finish_capture(
    ctx: &AdaptiveCaptureContext,
    request: &EpisodeCaptureRequest,
    kind: SegmentKind,
    mut attempt: StoredFingerprintAttempt,
    captured: CapturedFingerprint,
) -> Result<EpisodeEvidence, String> {
    let mut metrics = captured.metrics.clone();
    if let Some(tail) = &mut metrics.ffmpeg_stderr_tail {
        *tail = marker::fingerprint::sanitize_stderr(tail);
    }
    let metrics_json = serde_json::to_string(&metrics).map_err(|error| error.to_string())?;
    let sample = StoredFingerprintSample {
        sample_id: format!("samp-{}", Uuid::new_v4()),
        ledger_id: request.row.id.to_string(),
        source_version: request.source_version.clone(),
        capture_profile_key: request.capture_profile_key.clone(),
        kind: kind_name(kind).into(),
        window_start_ms: attempt.window_start_ms,
        window_end_ms: attempt.window_end_ms,
        pcm_duration_ms: captured.pcm_duration_ms,
        fingerprint: captured.words,
        captured_job_id: request.job_id.clone(),
        captured_at_ms: now_ms(),
        metrics_json: metrics_json.clone(),
    };
    attempt.finished_at_ms = Some(sample.captured_at_ms);
    attempt.status = "succeeded".into();
    attempt.metrics_json = metrics_json;
    let persisted = {
        let store = ctx.store.lock();
        ensure_source_available(&store, &request.row).and_then(|()| {
            store
                .complete_fingerprint_attempt_for_source(&attempt, &sample, &request.row.path)
                .map_err(|error| format!("Store error completing attempt: {error}"))
                .and_then(|accepted| {
                    if accepted {
                        Ok(())
                    } else {
                        Err("file_deleted: source removed before commit".into())
                    }
                })
        })
    };
    if let Err(error) = persisted {
        return finish_error(ctx, request, &mut attempt, error);
    }
    tracing::info!(job_id = %request.job_id, ledger_id = %request.row.id, ?kind,
        stage = %attempt.phase, elapsed_ms = now_ms().saturating_sub(attempt.started_at_ms),
        "声纹采集成功并已写入数据库");
    Ok(stored_sample_to_evidence(
        &sample,
        request.row.episode.unwrap_or(1),
        kind,
        request.media_duration_ms,
    ))
}

fn kind_name(kind: SegmentKind) -> &'static str {
    match kind {
        SegmentKind::Intro => "intro",
        SegmentKind::Outro => "outro",
    }
}

fn capture_error_kind(kind: &CaptureFailureKind) -> &'static str {
    match kind {
        CaptureFailureKind::Io => "io",
        CaptureFailureKind::Timeout => "timeout",
        CaptureFailureKind::EmptyAudio => "empty_audio",
        CaptureFailureKind::InvalidWindow => "invalid_window",
        CaptureFailureKind::Decode => "decode",
        CaptureFailureKind::Cancelled => "cancelled",
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}
