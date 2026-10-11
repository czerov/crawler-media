use std::collections::HashSet;

use domain::MediaId;
use marker::SegmentKind;
use marker::adaptive::{
    EpisodeDescriptor, EpisodeEvidence, SamplingPolicy, SourceCostSummary, TemplateContext,
    TemplateModel, build_season_models, select_seed_episodes,
};
use serde::{Deserialize, Serialize};

use super::{ProbeManager, ProbeUnit};
use crate::fingerprint_job::{
    AdaptiveCaptureContext, CapturePolicy, EpisodeCaptureRequest, EpisodeDetection,
    FingerprintCaptureProfile, capture_episode_adaptive, capture_profile_key,
};
use crate::scrape_store::ScrapeStoreExt;
use crate::store::{
    MarkerResultReplacement, StoredFingerprintModel, StoredFingerprintModelMember,
    StoredMediaMarker,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SeasonSamplingPlan {
    pub job_id: String,
    pub media_id: String,
    pub season: u32,
    pub sampling_mode: String,
    pub analysis_policy_key: String,
    pub seed_ledger_ids: Vec<String>,
}

/// Fixed collaborators of one season pipeline run, grouped to keep the steps readable.
struct SeasonPipeline<'a> {
    mgr: &'a ProbeManager,
    job_id: &'a str,
    media_id: MediaId,
    season: u32,
    profile_key: &'a str,
    policy: &'a SamplingPolicy,
    ctx: &'a AdaptiveCaptureContext,
    cost_summary: SourceCostSummary,
}

/// Results accumulated while the season units are captured one by one.
#[derive(Default)]
struct SeasonPipelineState {
    collected_evidences: Vec<EpisodeEvidence>,
    detections: Vec<EpisodeDetection>,
    templates: TemplateContext,
}

pub async fn run_adaptive_season_pipeline(
    mgr: &ProbeManager,
    job_id: &str,
    media_id: MediaId,
    season: u32,
    units: &[ProbeUnit],
) -> Result<MarkerResultReplacement, String> {
    let sampling_mode = effective_sampling_mode(mgr);
    let profile_key = capture_profile_key(&FingerprintCaptureProfile::default());
    let policy = SamplingPolicy::default();

    // Collect descriptors
    let descriptors = collect_episode_descriptors(mgr, units);
    let seed_ids = select_seed_episodes(&descriptors, &policy);
    let seed_set: HashSet<String> = seed_ids.iter().cloned().collect();

    let _plan = SeasonSamplingPlan {
        job_id: job_id.to_string(),
        media_id: media_id.to_string(),
        season,
        sampling_mode,
        analysis_policy_key: "default".to_string(),
        seed_ledger_ids: seed_ids,
    };

    let (gate, _leases) = super::progress::MetadataPriorityGate::for_rows(mgr, units)?;
    let ctx = adaptive_capture_context(mgr, std::sync::Arc::new(gate));
    let pipeline = SeasonPipeline {
        mgr,
        job_id,
        media_id,
        season,
        profile_key: &profile_key,
        policy: &policy,
        ctx: &ctx,
        cost_summary: SourceCostSummary::default(),
    };

    // 1. 种子单元优先处理，边采集边建模；2. 其余单元用已有模板验证。
    let mut state = SeasonPipelineState::default();
    for unit in order_seed_units_first(units, &seed_set) {
        let availability = crate::fingerprint_job::source::ensure_source_available(
            &pipeline.mgr.store.lock(),
            &unit.row,
        );
        if let Err(error) = availability {
            if !crate::fingerprint_job::source::is_file_deleted(&error) {
                return Err(error);
            }
            pipeline.mgr.cancel_unit(unit, "file_deleted");
            return Err(error);
        }
        let is_seed = seed_set.contains(&unit.row.id.to_string());
        match run_season_unit(&pipeline, unit, is_seed, &mut state).await {
            Ok(()) => {}
            Err(e) if e.starts_with("file_deleted") => {
                pipeline.mgr.cancel_unit(unit, "file_deleted");
                return Err(e);
            }
            Err(e) => return Err(e),
        }
    }

    ensure_season_sources(&pipeline, units)?;

    // 在所有单元采集完成后，使用全季收集的全部有效证据重新建模，
    // 发现可能存在的第二种变体或在种子阶段未识别出的模板，并重新验证所有单元。
    if state.collected_evidences.len() >= 2 {
        build_and_persist_templates(&pipeline, &state.collected_evidences, &mut state.templates);
    }
    reverify_detections(&pipeline, &state.templates, &mut state.detections);

    let (markers, chapter_updates) = build_marker_replacement(&pipeline, units, &state.detections)?;

    // 检查是否存在由于音频截断或覆盖不全被拒绝的证据：
    // 按 kind 独立检查，不能因为一个 kind（如 Intro）有标记就忽略另一个 kind（如 Outro）的音频截断失败。
    // 如果某个 kind 未能产出标记或模型，但该 kind 的采样证据中存在截断，必须报错退出，避免旧标记被清空。
    ensure_no_truncated_unmatched(units, &state.detections, &state.collected_evidences)?;

    // A manual refresh may publish only confirmed matches or confirmed absence.
    ensure_no_unresolved_forced_verification(units, &state.detections)?;

    ensure_season_sources(&pipeline, units)?;
    Ok(MarkerResultReplacement {
        media_id,
        season,
        markers,
        chapter_updates,
    })
}

fn ensure_season_sources(pipeline: &SeasonPipeline<'_>, units: &[ProbeUnit]) -> Result<(), String> {
    for unit in units {
        if pipeline.ctx.gate.work_control(&unit.row).is_cancelled() {
            return Err("Library probe source cancelled or changed".into());
        }
        let availability = crate::fingerprint_job::source::ensure_source_available(
            &pipeline.mgr.store.lock(),
            &unit.row,
        );
        if let Err(error) = availability {
            if crate::fingerprint_job::source::is_file_deleted(&error) {
                pipeline.mgr.cancel_unit(unit, "file_deleted");
            }
            return Err(error);
        }
    }
    Ok(())
}

fn effective_sampling_mode(mgr: &ProbeManager) -> String {
    let store = mgr.store.lock();
    store
        .get_scrape_config()
        .ok()
        .map(|c| c.effective.fingerprint_sampling_mode)
        .unwrap_or_else(|| "full_window".to_string())
}

fn collect_episode_descriptors(mgr: &ProbeManager, units: &[ProbeUnit]) -> Vec<EpisodeDescriptor> {
    let store = mgr.store.lock();
    units
        .iter()
        .map(|unit| {
            let ledger_id = unit.row.id.to_string();
            let path = std::path::Path::new(&unit.row.path);
            EpisodeDescriptor {
                ledger_id: ledger_id.clone(),
                episode: unit.row.episode.unwrap_or(1),
                source_version: crate::fingerprint_job::current_source_version(path),
                duration_ms: known_media_duration(&store, &ledger_id).unwrap_or(0),
            }
        })
        .collect()
}

fn known_media_duration(store: &crate::Store, ledger_id: &str) -> Option<i64> {
    store
        .get_media_info_cache_version(ledger_id)
        .ok()
        .flatten()
        .and_then(|cached| cached.format_duration_ms)
}

fn adaptive_capture_context(
    mgr: &ProbeManager,
    gate: std::sync::Arc<dyn crate::fingerprint_job::CaptureGate>,
) -> AdaptiveCaptureContext {
    AdaptiveCaptureContext {
        store: mgr.store.clone(),
        matcher: mgr.fingerprint_engine.clone(),
        capture: mgr.capture_engine.clone(),
        gate,
        timings: Some(mgr.timings.clone()),
    }
}

fn order_seed_units_first<'a>(
    units: &'a [ProbeUnit],
    seed_set: &HashSet<String>,
) -> Vec<&'a ProbeUnit> {
    let is_seed = |unit: &ProbeUnit| seed_set.contains(&unit.row.id.to_string());
    units
        .iter()
        .filter(|unit| is_seed(unit))
        .chain(units.iter().filter(|unit| !is_seed(unit)))
        .collect()
}

async fn run_season_unit(
    pipeline: &SeasonPipeline<'_>,
    unit: &ProbeUnit,
    is_seed: bool,
    state: &mut SeasonPipelineState,
) -> Result<(), String> {
    let ledger_id = unit.row.id.to_string();
    let path = std::path::Path::new(&unit.row.path);
    let capture_policy = if unit.reuse_fingerprint_cache {
        CapturePolicy::ReuseValid
    } else {
        CapturePolicy::Recapture
    };

    let req = EpisodeCaptureRequest {
        job_id: pipeline.job_id.to_string(),
        row: unit.row.clone(),
        source_version: crate::fingerprint_job::current_source_version(path),
        media_duration_ms: known_media_duration(&pipeline.mgr.store.lock(), &ledger_id),
        audio_stream_index: None,
        capture_profile_key: pipeline.profile_key.to_string(),
        policy: pipeline.policy.clone(),
        capture_policy,
        templates: state.templates.clone(),
        cost_summary: pipeline.cost_summary.clone(),
    };

    let detection = capture_episode_adaptive(pipeline.ctx, &req).await?;

    if let Some(evidence) = detection.intro_evidence.clone() {
        state.collected_evidences.push(evidence);
    }
    if let Some(evidence) = detection.outro_evidence.clone() {
        state.collected_evidences.push(evidence);
    }
    state.detections.push(detection);

    // 种子阶段收集到足够证据后立即建模，并回填已经产生的检测结果。
    if is_seed && state.collected_evidences.len() >= 2 {
        build_and_persist_templates(pipeline, &state.collected_evidences, &mut state.templates);
        reverify_detections(pipeline, &state.templates, &mut state.detections);
    }
    Ok(())
}

fn build_and_persist_templates(
    pipeline: &SeasonPipeline<'_>,
    collected_evidences: &[EpisodeEvidence],
    templates: &mut TemplateContext,
) {
    let comparison_start = std::time::Instant::now();
    let models = build_season_models(
        pipeline.mgr.fingerprint_engine.as_ref(),
        collected_evidences,
        pipeline.policy,
    );
    pipeline.mgr.timings.record_comparison(
        Some(pipeline.job_id),
        comparison_start.elapsed().as_millis() as u64,
    );

    for model in &models {
        for reference in &model.references {
            if let Some(evidence) = collected_evidences
                .iter()
                .find(|e| e.sample_id == reference.sample_id)
            {
                templates
                    .references
                    .insert(reference.sample_id.clone(), evidence.clone());
            }
        }
        persist_season_model(pipeline, model);
    }

    for evidence in collected_evidences {
        templates
            .references
            .insert(evidence.sample_id.clone(), evidence.clone());
    }
    templates.models = models;
}

fn persist_season_model(pipeline: &SeasonPipeline<'_>, model: &TemplateModel) {
    let model_json = serde_json::to_string(model).unwrap_or_else(|_| "{}".to_string());
    let stored_model = StoredFingerprintModel {
        model_id: model.model_id.clone(),
        media_id: pipeline.media_id.to_string(),
        season: pipeline.season,
        kind: match model.kind {
            SegmentKind::Intro => "intro".to_string(),
            SegmentKind::Outro => "outro".to_string(),
        },
        model_version: 1,
        membership_key: format!("{}:{}", pipeline.media_id, pipeline.season),
        policy_key: "default".to_string(),
        model_json,
        created_at_ms: now_ms(),
    };
    let members: Vec<StoredFingerprintModelMember> = model
        .references
        .iter()
        .map(|reference| StoredFingerprintModelMember {
            model_id: model.model_id.clone(),
            sample_id: reference.sample_id.clone(),
            ledger_id: reference.ledger_id.clone(),
            source_version: reference.source_version.clone(),
        })
        .collect();

    let store = pipeline.mgr.store.lock();
    if members.iter().any(|member| {
        store
            .get_ledger(&member.ledger_id)
            .ok()
            .flatten()
            .is_none_or(|row| {
                store
                    .get_media(row.media_id)
                    .ok()
                    .flatten()
                    .is_none_or(|media| {
                        store
                            .library_for_path_strict(std::path::Path::new(&row.path), media.kind)
                            .ok()
                            .flatten()
                            .is_some_and(|owner| {
                                !store.library_accepts_work(&owner.id).unwrap_or(false)
                            })
                    })
            })
    }) {
        tracing::warn!(
            job_id = pipeline.job_id,
            "cancelled Library season model publication"
        );
        return;
    }
    if let Err(error) = store.put_fingerprint_model(&stored_model, &members) {
        tracing::warn!(error = %error, "failed to persist fingerprint season model");
    }
}

fn reverify_detections(
    pipeline: &SeasonPipeline<'_>,
    templates: &TemplateContext,
    detections: &mut [EpisodeDetection],
) {
    for detection in detections.iter_mut() {
        verify_detection_kind(
            pipeline,
            templates,
            &detection.intro_evidence,
            &mut detection.intro_match,
            &mut detection.intro_outcome,
        );
        verify_detection_kind(
            pipeline,
            templates,
            &detection.outro_evidence,
            &mut detection.outro_match,
            &mut detection.outro_outcome,
        );
    }
}

/// 当标准验证返回 `Verified` 时回填匹配区间，并记录最新验证结果 outcome。
/// 若最终验证为 `NeedsFullWindow`、`NoMatch` 或 `SamplingLimit`，必须清空先前的暂存匹配（matched），
/// 防止种子阶段的早熟暂存结果绕过最终裁决被错误发布。
fn verify_detection_kind(
    pipeline: &SeasonPipeline<'_>,
    templates: &TemplateContext,
    evidence: &Option<EpisodeEvidence>,
    matched: &mut Option<marker::adaptive::VerifiedInterval>,
    outcome_dest: &mut Option<marker::adaptive::VerificationOutcome>,
) {
    let Some(evidence) = evidence else { return };

    let comparison_start = std::time::Instant::now();
    let outcome = marker::adaptive::verify_template_window(
        pipeline.mgr.fingerprint_engine.as_ref(),
        evidence,
        templates,
        pipeline.policy,
    );
    match outcome {
        marker::adaptive::VerificationOutcome::Verified(ref verified) => {
            *matched = Some(verified.clone());
        }
        _ => {
            *matched = None;
        }
    }
    *outcome_dest = Some(outcome);
    pipeline.mgr.timings.record_comparison(
        Some(pipeline.job_id),
        comparison_start.elapsed().as_millis() as u64,
    );
}

#[path = "season_versions.rs"]
mod versions;
use versions::validate_version_outcomes;

fn build_marker_replacement(
    pipeline: &SeasonPipeline<'_>,
    units: &[ProbeUnit],
    detections: &[EpisodeDetection],
) -> Result<
    (
        Vec<StoredMediaMarker>,
        Vec<(String, Vec<marker::ChapterMarker>)>,
    ),
    String,
> {
    validate_version_outcomes(detections)?;
    let mut marker_map: std::collections::BTreeMap<u32, StoredMediaMarker> =
        std::collections::BTreeMap::new();
    let mut chapter_updates = Vec::new();
    let updated_at = now_secs();
    let store = pipeline.mgr.store.lock();

    for unit in units {
        let ledger_id = unit.row.id.to_string();
        let episode = unit.row.episode.unwrap_or(1);
        let detection = find_detection(detections, &ledger_id, episode);
        let previous = store
            .get_media_marker(pipeline.media_id, Some(pipeline.season), Some(episode))
            .ok()
            .flatten();

        let (intro_start_ms, intro_end_ms) =
            detection_segment_range(detection, SegmentKind::Intro, previous.as_ref());
        let (outro_start_ms, outro_end_ms) =
            detection_segment_range(detection, SegmentKind::Outro, previous.as_ref());
        let source = marker_source(detection, previous.as_ref());

        if intro_start_ms.is_some() || outro_start_ms.is_some() {
            let candidate_marker = StoredMediaMarker {
                media_id: pipeline.media_id,
                season: pipeline.season,
                episode,
                intro_start_ms,
                intro_end_ms,
                outro_start_ms,
                outro_end_ms,
                source,
                locked: false,
                updated_at,
            };

            match marker_map.entry(episode) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(candidate_marker);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    let existing = entry.get_mut();
                    // Prefer marker with more detected segments (both intro and outro vs one)
                    let existing_count = existing.intro_start_ms.is_some() as usize
                        + existing.outro_start_ms.is_some() as usize;
                    let candidate_count = candidate_marker.intro_start_ms.is_some() as usize
                        + candidate_marker.outro_start_ms.is_some() as usize;
                    if candidate_count > existing_count {
                        *existing = candidate_marker;
                    }
                }
            }
        }

        let existing = store
            .get_cached_chapters(&ledger_id)
            .ok()
            .flatten()
            .unwrap_or_default();
        let duration_ms = known_media_duration(&store, &ledger_id);
        let complete = marker::build_complete_timeline_chapters(
            &existing,
            intro_start_ms.zip(intro_end_ms),
            display_outro_range(outro_start_ms, outro_end_ms),
            duration_ms,
        );
        chapter_updates.push((ledger_id, complete));
    }

    let markers = marker_map.into_values().collect();
    Ok((markers, chapter_updates))
}

fn detection_segment_range(
    detection: Option<&EpisodeDetection>,
    kind: SegmentKind,
    previous: Option<&StoredMediaMarker>,
) -> (Option<i64>, Option<i64>) {
    let (matched, sampled, outcome, previous) = match kind {
        SegmentKind::Intro => (
            detection.and_then(|d| d.intro_match.as_ref()),
            detection.is_some_and(|d| d.intro_evidence.is_some()),
            detection.and_then(|d| d.intro_outcome.as_ref()),
            previous.map(|m| (m.intro_start_ms, m.intro_end_ms)),
        ),
        SegmentKind::Outro => (
            detection.and_then(|d| d.outro_match.as_ref()),
            detection.is_some_and(|d| d.outro_evidence.is_some()),
            detection.and_then(|d| d.outro_outcome.as_ref()),
            previous.map(|m| (m.outro_start_ms, m.outro_end_ms)),
        ),
    };
    resolve_segment_range(
        matched.map(|v| (v.start_ms, v.end_ms)),
        sampled,
        outcome,
        previous,
    )
}

fn marker_source(
    detection: Option<&EpisodeDetection>,
    previous: Option<&StoredMediaMarker>,
) -> String {
    if detection.is_some_and(|d| d.intro_match.is_some() || d.outro_match.is_some()) {
        "fingerprint_adaptive".to_string()
    } else {
        previous
            .map(|m| m.source.clone())
            .unwrap_or_else(|| "fingerprint_adaptive".to_string())
    }
}

/// 结合标准验证结果决定某个 kind 最终应当写入标记的时间区间。
///
/// - 标准验证返回 `Verified`：采用本轮识别到的新区间。
/// - 标准验证在完整窗口上明确返回 `NoMatch`：确认该剧集没有此片段，允许清空旧值。
/// - 其余情况（未采样、需要完整窗口、采样受限）都属于"无法判定"：
///   必须保留数据库中已有的旧值（可能只有起始时间），
///   绝不能把"无法判定"当成"确认无此片段"发布，从而误删旧标记。
fn resolve_segment_range(
    matched: Option<(i64, i64)>,
    sampled: bool,
    outcome: Option<&marker::adaptive::VerificationOutcome>,
    previous: Option<(Option<i64>, Option<i64>)>,
) -> (Option<i64>, Option<i64>) {
    if let Some((start_ms, end_ms)) = matched {
        return (Some(start_ms), Some(end_ms));
    }
    let confirmed_absent = sampled
        && matches!(
            outcome,
            Some(marker::adaptive::VerificationOutcome::NoMatch { .. })
        );
    if confirmed_absent {
        return (None, None);
    }
    previous.unwrap_or((None, None))
}

/// 章节展示需要一个结束时间；标记本身允许只有片尾起始时间，
/// 因此这里为章节补一个仅供展示的兜底结束时间，不写回标记事实。
fn display_outro_range(start_ms: Option<i64>, end_ms: Option<i64>) -> Option<(i64, i64)> {
    match (start_ms, end_ms) {
        (Some(start), Some(end)) if end > start => Some((start, end)),
        (Some(start), None) => Some((start, start + 60_000)),
        _ => None,
    }
}

fn ensure_no_truncated_unmatched(
    units: &[ProbeUnit],
    detections: &[EpisodeDetection],
    collected_evidences: &[EpisodeEvidence],
) -> Result<(), String> {
    for kind in [SegmentKind::Intro, SegmentKind::Outro] {
        if has_truncated_unmatched(units, detections, collected_evidences, kind) {
            let kind_name = match kind {
                SegmentKind::Intro => "片头",
                SegmentKind::Outro => "片尾",
            };
            return Err(format!(
                "{kind_name}采集音频数据不完整或被截断，未能为全部剧集形成有效标记，保留现有标记"
            ));
        }
    }
    Ok(())
}

/// 某集该 kind 没有标记，但该集同 kind 的采样证据被截断或时长未知时为 true。
fn has_truncated_unmatched(
    units: &[ProbeUnit],
    detections: &[EpisodeDetection],
    collected_evidences: &[EpisodeEvidence],
    kind: SegmentKind,
) -> bool {
    units.iter().any(|unit| {
        let episode = unit.row.episode.unwrap_or(1);
        let ledger_id = unit.row.id.to_string();
        let detection = find_detection(detections, &ledger_id, episode);
        let has_detection = match kind {
            SegmentKind::Intro => detection.and_then(|d| d.intro_match.as_ref()).is_some(),
            SegmentKind::Outro => detection.and_then(|d| d.outro_match.as_ref()).is_some(),
        };
        if has_detection {
            return false;
        }
        // Episode has no detection for this kind. Check if it had a truncated or unknown PCM evidence.
        collected_evidences.iter().any(|evidence| {
            if evidence.episode != episode || evidence.kind != kind {
                return false;
            }
            match evidence.capture.pcm_duration_ms {
                Some(pcm_ms) => pcm_ms + 1000 < evidence.capture.window.duration_ms(),
                None => true,
            }
        })
    })
}

fn find_detection<'a>(
    detections: &'a [EpisodeDetection],
    ledger_id: &str,
    episode: u32,
) -> Option<&'a EpisodeDetection> {
    detections
        .iter()
        .find(|detection| detection.ledger_id == ledger_id)
        .or_else(|| {
            detections
                .iter()
                .find(|detection| detection.episode == episode)
        })
}

fn ensure_no_unresolved_forced_verification(
    units: &[ProbeUnit],
    detections: &[EpisodeDetection],
) -> Result<(), String> {
    if !units.iter().any(|unit| unit.marker_refresh_id.is_some()) {
        return Ok(());
    }
    for detection in detections {
        for (kind, outcome) in [
            ("intro", &detection.intro_outcome),
            ("outro", &detection.outro_outcome),
        ] {
            match outcome {
                Some(
                    marker::adaptive::VerificationOutcome::Verified(_)
                    | marker::adaptive::VerificationOutcome::NoMatch { .. },
                ) => {}
                other => {
                    return Err(format!(
                        "verification_unresolved: episode={} kind={kind} outcome={other:?}; keeping existing markers",
                        detection.episode
                    ));
                }
            }
        }
    }
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}
