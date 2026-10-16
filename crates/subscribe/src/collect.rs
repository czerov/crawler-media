use std::fs;
use std::path::{Path, PathBuf};

use domain::{Confidence, LedgerId, LedgerRow, QualitySource, Release};
use downloader::Downloader;
use filter::ScoredTorrent;
use hooks::{HookEvent, Step};
use library::{MediaProbe, resolve_mode, scrape_beside, transfer_file_controlled};

use crate::add::Added;
use crate::choose::{covered_slots, is_complete};
use crate::facts::{QualityFact, SubscribeFacts};
use crate::{LedgerSource, RunOutcome, SubscribeError};

/// Durable final-path reservation around filesystem publication. Implementations
/// must not perform filesystem work while recording or completing a row.
pub trait TransferRecorder {
    fn prepare(&self, row: &LedgerRow, source: &Path, mode: library::TransferMode)
        -> Result<LedgerRow, SubscribeError>;
    fn published(&self, row: &LedgerRow) -> Result<(), SubscribeError>;
}

pub fn collect_completed<D: Downloader + ?Sized>(
    added: Added<'_, D>,
    probe: &dyn MediaProbe,
) -> Result<RunOutcome, SubscribeError> {
    collect_completed_with_destinations(added, probe, &[])
}

pub fn collect_completed_with_destinations<D: Downloader + ?Sized>(
    added: Added<'_, D>,
    probe: &dyn MediaProbe,
    existing: &[crate::collection_destinations::DestinationMapping],
) -> Result<RunOutcome, SubscribeError> {
    collect_completed_controlled(added, probe, existing, &library::WorkControl::default())
}

pub fn collect_completed_controlled<D: Downloader + ?Sized>(
    added: Added<'_, D>,
    probe: &dyn MediaProbe,
    existing: &[crate::collection_destinations::DestinationMapping],
    control: &library::WorkControl,
) -> Result<RunOutcome, SubscribeError> {
    collect_completed_recorded(added, probe, existing, control, None)
}

pub fn collect_completed_recorded<D: Downloader + ?Sized>(
    mut added: Added<'_, D>,
    probe: &dyn MediaProbe,
    existing: &[crate::collection_destinations::DestinationMapping],
    control: &library::WorkControl,
    recorder: Option<&dyn TransferRecorder>,
) -> Result<RunOutcome, SubscribeError> {
    control.check()?;
    let mut ledger = Vec::new();
    let mut ledger_sources = Vec::new();
    let mut removed_paths = Vec::new();
    let mut transferred_enclosures = Vec::new();
    let mut rejected_enclosures = Vec::new();
    let mut awaiting_probe_enclosures = Vec::new();
    let mut collection_errors = Vec::new();

    // 先依据现有已确认的目标映射刷新/协调 facts，确保即便 Downloader 在 Move 模式下不再列出视频，
    // 本地持久化的映射依然能驱动并修复 facts
    for mapping in existing {
        if !mapping.transfer_pending && mapping.destination_path.is_file() {
            reconcile_mapping_facts(&mut added.input.facts, added.input.subscribe, mapping, 0);
        }
    }

    let chosen = std::mem::take(&mut added.chosen);
    for scored in chosen {
        control.check()?;
        collect_scored_torrent(
            &mut added,
            probe,
            existing,
            &scored,
            &mut ledger,
            &mut ledger_sources,
            &mut removed_paths,
            &mut transferred_enclosures,
            &mut rejected_enclosures,
            &mut awaiting_probe_enclosures,
            &mut collection_errors,
            control,
            recorder,
        );
    }
    control.check()?;
    Ok(RunOutcome {
        completed: is_complete(added.input.subscribe, &added.input.facts),
        facts: added.input.facts,
        ledger,
        ledger_sources,
        removed_paths,
        transferred_enclosures,
        rejected_enclosures,
        awaiting_probe_enclosures,
        collection_errors,
        submission_errors: added.add_errors,
        torrents_added: added.outcome.torrents_added,
    })
}

fn collect_scored_torrent<D: Downloader + ?Sized>(
    added: &mut Added<'_, D>,
    probe: &dyn MediaProbe,
    existing: &[crate::collection_destinations::DestinationMapping],
    scored: &ScoredTorrent,
    ledger: &mut Vec<LedgerRow>,
    ledger_sources: &mut Vec<LedgerSource>,
    removed_paths: &mut Vec<String>,
    transferred_enclosures: &mut Vec<String>,
    rejected_enclosures: &mut Vec<String>,
    awaiting_probe_enclosures: &mut Vec<String>,
    collection_errors: &mut Vec<String>,
    control: &library::WorkControl,
    recorder: Option<&dyn TransferRecorder>,
) {
    // Review H-3: a claimed user task's files are never moved away; Move
    // degrades to Hardlink (copy fallback) for this torrent.
    let saved_mode = added.input.transfer_mode;
    if matches!(saved_mode, Some(library::TransferMode::Move))
        && added
            .input
            .downloader
            .is_claimed(&scored.torrent)
            .unwrap_or(true)
    {
        tracing::info!(torrent = %scored.torrent.title, "认领的用户任务：Move 降级为硬链接，不移走用户文件");
        added.input.transfer_mode = Some(library::TransferMode::Hardlink);
    }
    collect_scored_torrent_inner(
        added,
        probe,
        existing,
        scored,
        ledger,
        ledger_sources,
        removed_paths,
        transferred_enclosures,
        rejected_enclosures,
        awaiting_probe_enclosures,
        collection_errors,
        control,
        recorder,
    );
    added.input.transfer_mode = saved_mode;
}

#[allow(clippy::too_many_arguments)]
fn collect_scored_torrent_inner<D: Downloader + ?Sized>(
    added: &mut Added<'_, D>,
    probe: &dyn MediaProbe,
    existing: &[crate::collection_destinations::DestinationMapping],
    scored: &ScoredTorrent,
    ledger: &mut Vec<LedgerRow>,
    ledger_sources: &mut Vec<LedgerSource>,
    removed_paths: &mut Vec<String>,
    transferred_enclosures: &mut Vec<String>,
    rejected_enclosures: &mut Vec<String>,
    awaiting_probe_enclosures: &mut Vec<String>,
    collection_errors: &mut Vec<String>,
    control: &library::WorkControl,
    recorder: Option<&dyn TransferRecorder>,
) {
    let mut vetoed = false;
    // Review M-4: a veto caused by an unfinished probe; the row stays active.
    let mut awaiting_probe = false;
    // M2: video files skipped because their slots were already owned.
    let mut duplicate = false;
    let errors_before_torrent = collection_errors.len();
    // D-L1: finished files are collected while the rest of the task is still
    // downloading; the task only counts as transferred once it is done.
    // Move would take files away from a running task, so it waits.
    let (completed_files, task_done) = match added
        .input
        .downloader
        .collectable_files(&scored.torrent)
    {
        Ok((files, done))
            if done || !matches!(added.input.transfer_mode, Some(library::TransferMode::Move)) =>
        {
            (files, done)
        }
        Ok(_) => (Vec::new(), false),
        Err(error) => {
            tracing::error!(torrent = %scored.torrent.title, error = %error, "读取已完成下载文件失败");
            collection_errors.push(format!("{}: {error}", scored.torrent.title));
            return;
        }
    };
    let (videos, others): (Vec<_>, Vec<_>) = completed_files
        .into_iter()
        .partition(|path| crate::sidecars::is_video(path));
    let (subs, _junk): (Vec<_>, Vec<_>) = others
        .into_iter()
        .partition(|path| crate::sidecars::is_subtitle(path));
    let single_file = videos.len() == 1;
    let reported_any = !videos.is_empty() || !subs.is_empty();
    let mut torrent_destinations = Vec::new();
    for src in videos {
        if control.is_cancelled() {
            return;
        }
        let source_path = src.display().to_string();
        let mapping = existing.iter().find(|m| m.source_path == source_path);
        if let Some(mapping) = mapping.filter(|mapping| !mapping.transfer_pending) {
            if mapping.destination_path.is_file() {
                torrent_destinations.push(mapping.clone());
                reconcile_mapping_facts(
                    &mut added.input.facts,
                    added.input.subscribe,
                    mapping,
                    scored.score,
                );
            } else {
                tracing::error!(source = %source_path, destination = %mapping.destination_path.display(), "已拥有的视频目标缺失，不自动恢复用户删除的文件");
                collection_errors.push(format!(
                    "missing owned destination: {}",
                    mapping.destination_path.display()
                ));
            }
            continue;
        }
        match collect_one(
            added,
            probe,
            scored,
            src,
            single_file,
            ledger,
            ledger_sources,
            removed_paths,
            &mut vetoed,
            &mut awaiting_probe,
            &mut duplicate,
            control,
            recorder,
            mapping.filter(|mapping| mapping.transfer_pending),
        ) {
            Ok(Some(dest)) => torrent_destinations.push(
                crate::collection_destinations::DestinationMapping::new(source_path, dest),
            ),
            Ok(None) => {}
            Err(error) => {
                tracing::error!(torrent = %scored.torrent.title, source = %source_path, error = %error, "收集视频文件失败");
                collection_errors.push(format!(
                    "{} ({}): {error}",
                    scored.torrent.title, source_path
                ));
            }
        }
    }
    let mut sidecar_destinations = torrent_destinations.clone();
    for mapping in existing {
        if mapping.destination_path.is_file()
            && !sidecar_destinations
                .iter()
                .any(|known| known.source_path == mapping.source_path)
        {
            sidecar_destinations.push(mapping.clone());
        }
    }
    // M2: subtitles of a pure duplicate have no new video to sit beside.
    let subs = if duplicate && torrent_destinations.is_empty() {
        Vec::new()
    } else {
        subs
    };
    let subs_count = subs.len();
    for src in subs {
        if control.is_cancelled() {
            return;
        }
        match crate::sidecars::place_mapped_subtitle_controlled(
            &src,
            &sidecar_destinations,
            added.input.transfer_mode,
            control,
        ) {
            Ok(()) => {}
            Err(error) => {
                tracing::error!(source = %src.display(), error = %error, "摆放字幕失败");
                collection_errors.push(format!("subtitle {}: {error}", src.display()));
            }
        }
    }
    // A download that only duplicated owned episodes (M2) is finished too.
    let has_work = !torrent_destinations.is_empty() || subs_count > 0 || duplicate;
    if task_done
        && reported_any
        && has_work
        && !awaiting_probe
        && collection_errors.len() == errors_before_torrent
    {
        transferred_enclosures.push(scored.torrent.enclosure.clone());
    }
    if vetoed && !rejected_enclosures.contains(&scored.torrent.enclosure) {
        rejected_enclosures.push(scored.torrent.enclosure.clone());
    }
    if awaiting_probe
        && !vetoed
        && !awaiting_probe_enclosures.contains(&scored.torrent.enclosure)
    {
        awaiting_probe_enclosures.push(scored.torrent.enclosure.clone());
    }
}

fn reconcile_mapping_facts(
    facts: &mut SubscribeFacts,
    subscribe: &domain::Subscribe,
    mapping: &crate::collection_destinations::DestinationMapping,
    score: i32,
) {
    let dest_str = mapping.destination_path.display().to_string();
    let file_release = mapping.quality.clone().unwrap_or_else(|| {
        release::parse(
            mapping
                .destination_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default(),
        )
    });
    if facts.quality(&dest_str).is_none() {
        facts.set_quality(dest_str.clone(), file_release.clone());
    }
    let slots = if !mapping.slots.is_empty() {
        mapping.slots.clone()
    } else {
        covered_slots(subscribe, &file_release)
    };
    for (s, e) in slots {
        let existing_fact = facts.get(s, e);
        let is_stale = existing_fact
            .as_ref()
            .and_then(|f| f.path.as_deref())
            .map_or(false, |p| !std::path::Path::new(p).exists());
        if existing_fact.is_none() || is_stale {
            facts.replace(
                s,
                e,
                QualityFact {
                    score,
                    path: Some(dest_str.clone()),
                },
            );
        } else if existing_fact.as_ref().and_then(|f| f.path.as_deref()) == Some(dest_str.as_str())
        {
            facts.upsert(
                s,
                e,
                QualityFact {
                    score,
                    path: Some(dest_str.clone()),
                },
            );
        }
    }
}

fn collect_one<D: Downloader + ?Sized>(
    added: &mut Added<'_, D>,
    probe: &dyn MediaProbe,
    scored: &ScoredTorrent,
    src: PathBuf,
    single_file: bool,
    ledger: &mut Vec<LedgerRow>,
    ledger_sources: &mut Vec<LedgerSource>,
    removed_paths: &mut Vec<String>,
    vetoed: &mut bool,
    awaiting_probe: &mut bool,
    duplicate: &mut bool,
    control: &library::WorkControl,
    recorder: Option<&dyn TransferRecorder>,
    pending: Option<&crate::collection_destinations::DestinationMapping>,
) -> Result<Option<PathBuf>, SubscribeError> {
    let file_release = release::parse(
        src.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default(),
    );
    // G-H2b: an absolute-numbered file maps onto the catalog season too.
    // Only for torrents whose own title has no season marker.
    let (file_release, torrent_release) = match added.input.absolute {
        Some(map) if !release::has_explicit_season(&scored.torrent.title) => {
            (map.apply(&file_release), map.apply(&scored.release))
        }
        _ => (file_release, scored.release.clone()),
    };
    let release = match crate::file_identity::resolve_file_release(
        added.input.subscribe,
        added.input.media,
        &src,
        &file_release,
        &torrent_release,
        single_file,
    ) {
        Some(release) => release,
        None => return Ok(None),
    };
    // Facts paths are canonical: retries must not create collision copies or
    // restore user-deleted files. Unapproved Wash-cut arrivals never replace owned slots.
    let slots = covered_slots(added.input.subscribe, &release);
    let approved = crate::slot_replacement::is_replacement_approved(
        added.input.subscribe,
        added.input.wash_filter,
        &added.input.facts,
        scored,
        &slots,
    );
    let has_existing = slots
        .iter()
        .any(|(season, episode)| added.input.facts.get(*season, *episode).is_some());
    // Only unapproved Wash-cut arrivals are blocked.
    if added.input.subscribe.wash_cut && has_existing && !approved {
        return Ok(None);
    }
    // M2: without wash-cut an arrival whose every slot already has a file
    // owned before this run is a duplicate (e.g. the owned episode inside a
    // range pack). Files published earlier in this same run (extras) coexist.
    let all_owned = !slots.is_empty()
        && slots.iter().all(|(season, episode)| {
            added
                .input
                .facts
                .get(*season, *episode)
                .and_then(|fact| fact.path.as_deref())
                .is_some_and(|path| {
                    Path::new(path).exists() && !ledger.iter().any(|row| row.path == path)
                })
        });
    if !added.input.subscribe.wash_cut && all_owned {
        tracing::info!(torrent = %scored.torrent.title, file = %src.display(), "非洗版：槽位已有在库文件，跳过转存");
        *duplicate = true;
        return Ok(None);
    }
    // Review N-1: an arrival that needs S1 approval is probed at the source
    // before anything is published, so an unprobeable or non-upgrade file
    // never enters the Library (no publish/retract churn, no copies).
    let pre_probed = if added.input.subscribe.wash_cut && has_existing {
        let (quality, source, confidence) = quality_for(probe, &src, &scored.release, control);
        control.check()?;
        if source != QualitySource::Probe {
            tracing::warn!(file = %src.display(), "源文件 probe 未完成，暂不转入媒体库，等待下一轮");
            *awaiting_probe = true;
            return Ok(None);
        }
        let (ok, _) = replacement_survives_probe(added, scored, &slots, approved, &quality, true);
        if !ok {
            tracing::warn!(file = %src.display(), "源文件 probe 后不构成升级，不转入媒体库");
            *vetoed = true;
            return Ok(None);
        }
        Some((quality, source, confidence))
    } else {
        None
    };
    // Main's reservation: a retried transfer reuses its reserved destination.
    // Otherwise a name can be taken between choosing it and publishing it
    // (another worker, the user): publication never overwrites (D-H3), so pick
    // the next free name and retry a few times (M8).
    let (dest, mut row) = match pending {
        Some(mapping) => {
            let dest = mapping.destination_path.clone();
            let row = reserve_and_publish(
                added, scored, &release, &src, &dest, true, control, recorder,
            )?;
            (dest, row)
        }
        None => {
            let mut attempts = 0;
            loop {
                let dest = dest_path(
                    added.input.library_root,
                    added.input.media,
                    &release,
                    &src,
                    added.input.naming,
                )?;
                if added.input.facts.entries().any(|(_, f)| {
                    f.path
                        .as_deref()
                        .is_some_and(|p| p == dest.to_string_lossy())
                }) {
                    return Ok(None);
                }
                match reserve_and_publish(
                    added, scored, &release, &src, &dest, false, control, recorder,
                ) {
                    Err(SubscribeError::Library(library::LibraryError::Io(error)))
                        if error.kind() == std::io::ErrorKind::AlreadyExists && attempts < 3 =>
                    {
                        attempts += 1;
                        tracing::warn!(dest = %dest.display(), "目标文件在发布前出现，改用下一个可用文件名");
                    }
                    result => break (dest, result?),
                }
            }
        }
    };
    let (quality, source, confidence) = match pre_probed {
        Some(probed) => probed,
        None => quality_for(probe, &dest, &scored.release, control),
    };
    control.check()?;
    // Final deletion approval uses actual (probed) quality, never the download
    // title alone: an unprobed arrival cannot replace an owned slot (S1).
    let (approved, actual_score) = replacement_survives_probe(
        added,
        scored,
        &slots,
        approved,
        &quality,
        source == QualitySource::Probe,
    );
    if added.input.subscribe.wash_cut && has_existing && !approved {
        // Review M-4: only a finished probe that disproves the upgrade is a
        // final rejection; an unfinished probe waits for the next round.
        let probed = source == QualitySource::Probe;
        if probed {
            *vetoed = true;
        } else {
            *awaiting_probe = true;
        }
        // Roll back the side-by-side publish (queued deletion of file and
        // ledger row). Move took the file out of the download: keep it.
        let mode = resolve_mode(&src, added.input.library_root, added.input.transfer_mode);
        if !matches!(mode, library::TransferMode::Move) {
            tracing::warn!(dest = %dest.display(), probed, "替换未获批准，撤回已转入的并列文件");
            removed_paths.push(dest.display().to_string());
            return Ok(None);
        }
        tracing::warn!(
            dest = %dest.display(),
            resolution = ?quality.resolution,
            codec = ?quality.codec,
            "probe 后的实际质量不构成升级，保留已转入的新文件，不删除在位版本"
        );
    }
    // 拒绝的 Wash-cut 只能登记并列文件，不能借标题高分改写已有槽位。
    let committed_slots: Vec<_> = slots
        .iter()
        .copied()
        .filter(|(s, e)| {
            !added.input.subscribe.wash_cut || approved || added.input.facts.get(*s, *e).is_none()
        })
        // M2: non-wash never repoints a slot owned before this run.
        .filter(|(s, e)| {
            added.input.subscribe.wash_cut
                || !added
                    .input
                    .facts
                    .get(*s, *e)
                    .and_then(|fact| fact.path.as_deref())
                    .is_some_and(|path| !ledger.iter().any(|row| row.path == path))
        })
        .collect();
    commit_video_facts(
        added,
        &committed_slots,
        actual_score,
        &dest,
        approved,
        removed_paths,
    )?;
    let ledger_path = dest.display().to_string();
    let mut owned_quality = scored.release.clone();
    owned_quality.resolution = quality.resolution.clone();
    owned_quality.codec = quality.codec.clone();
    owned_quality.hdr = quality.hdr.clone();
    added
        .input
        .facts
        .set_quality(ledger_path.clone(), owned_quality);
    ledger_sources.push(LedgerSource {
        ledger_path: ledger_path.clone(),
        source_path: src.display().to_string(),
    });
    row.resolution = quality.resolution;
    row.codec = quality.codec;
    row.hdr = quality.hdr;
    row.quality_source = source;
    row.confidence = confidence;
    row.filter_score = Some(actual_score);
    ledger.push(row);
    Ok(Some(dest))
}

fn reserve_and_publish<D: Downloader + ?Sized>(
    added: &Added<'_, D>, scored: &ScoredTorrent, release: &Release,
    src: &Path, dest: &Path, pending: bool, control: &library::WorkControl,
    recorder: Option<&dyn TransferRecorder>,
) -> Result<LedgerRow, SubscribeError> {
    let mut row = LedgerRow {
        id: LedgerId::new(), media_id: added.input.media.id,
        path: dest.display().to_string(), season: release.season, episode: release.episode,
        resolution: release.resolution.clone(), codec: release.codec.clone(), hdr: release.hdr.clone(),
        quality_source: QualitySource::Release, confidence: release.confidence,
        filter_score: Some(scored.score),
    };
    if let Some(recorder) = recorder {
        row = recorder.prepare(&row, src,
            resolve_mode(src, added.input.library_root, added.input.transfer_mode))?;
    }
    if !pending || !dest.is_file() {
        publish_video(added, src, dest, control)?;
    }
    if let Some(recorder) = recorder {
        recorder.published(&row)?;
    }
    Ok(row)
}

fn publish_video<D: Downloader + ?Sized>(
    added: &Added<'_, D>,
    src: &Path,
    dest: &Path,
    control: &library::WorkControl,
) -> Result<(), SubscribeError> {
    let mode = resolve_mode(src, added.input.library_root, added.input.transfer_mode);
    tracing::info!(src = %src.display(), dest = %dest.display(), ?mode, "将已完成的媒体文件转入媒体库");
    emit(added.input.hooks, Step::Rename)?;
    emit(added.input.hooks, Step::Transfer)?;
    transfer_file_controlled(src, dest, mode, control).map_err(|e| {
        tracing::error!(src = %src.display(), dest = %dest.display(), error = %e, "文件转移失败");
        SubscribeError::Library(e)
    })?;
    control.check()?;
    emit(added.input.hooks, Step::Scrape)?;
    control.check()?;
    if let Err(error) = control.publish(|| {
        scrape_beside(dest, added.input.media, added.input.scrape, None).map_err(|e| e.to_string())
    }) {
        control.check()?;
        tracing::warn!(dest = %dest.display(), %error, "刮削写入元数据失败，但媒体文件已就位，继续入账");
    }
    Ok(())
}

fn commit_video_facts<D: Downloader + ?Sized>(
    added: &mut Added<'_, D>,
    slots: &[(Option<u32>, Option<u32>)],
    score: i32,
    dest: &Path,
    approved: bool,
    removed_paths: &mut Vec<String>,
) -> Result<(), SubscribeError> {
    let previous: Vec<_> = slots
        .iter()
        .filter_map(|(s, e)| added.input.facts.get(*s, *e).and_then(|f| f.path.clone()))
        .collect();
    for (s, e) in slots {
        let fact = QualityFact {
            score,
            path: Some(dest.display().to_string()),
        };
        if approved {
            added.input.facts.replace(*s, *e, fact);
        } else {
            added.input.facts.upsert(*s, *e, fact);
        }
    }
    if approved && added.input.subscribe.wash_cut && !added.input.subscribe.keep_old_versions {
        for path in previous {
            let still_referenced = added
                .input
                .facts
                .entries()
                .any(|(_, fact)| fact.path.as_deref() == Some(path.as_str()));
            if still_referenced {
                continue;
            }
            if Path::new(&path) != dest {
                if !added.input.preserve_removed {
                    // Best effort here, but never silent: a failure to unlink is
                    // logged and the path is still handed to the caller, whose
                    // commit step retries the delete together with the ledger row.
                    // Returning early would persist facts pointing at `dest` while
                    // no ledger row exists for it, leaving the task silently active.
                    match fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            tracing::debug!(path = %path, "被替换的 Library 文件已不存在，仍清理台账");
                        }
                        Err(error) => {
                            tracing::error!(path = %path, %error, "删除被替换 Library 文件失败，交由提交阶段重试");
                        }
                    }
                }
                removed_paths.push(path);
            }
        }
    }
    Ok(())
}

fn dest_path(
    root: &Path,
    media: &domain::Media,
    release: &Release,
    src: &Path,
    naming: Option<&str>,
) -> Result<PathBuf, SubscribeError> {
    let pattern = naming.unwrap_or_else(|| library::default_pattern(media.kind));
    let dest = library::render_path(root, pattern, media, release, src)?;
    if !dest.exists() {
        return Ok(dest);
    }
    // M8: never hand back an occupied alternate name; walk `-2`, `-3`, … until
    // a free slot. An alternate that already is this very file (retry after a
    // hardlink import) is reused so retries stay idempotent.
    let stem = dest.file_stem().and_then(|s| s.to_str()).unwrap_or("media");
    let src_stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("alt");
    let ext = dest.extension().and_then(|s| s.to_str()).unwrap_or("bin");
    for n in 1..=MAX_ALTERNATE_NAMES {
        let name = if n == 1 {
            format!("{stem}-{src_stem}.{ext}")
        } else {
            format!("{stem}-{src_stem}-{n}.{ext}")
        };
        let candidate = dest.with_file_name(name);
        if std::fs::symlink_metadata(&candidate).is_err() || same_file(src, &candidate) {
            return Ok(candidate);
        }
    }
    Err(SubscribeError::Library(library::LibraryError::Io(
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("no free alternate name for {}", dest.display()),
        ),
    )))
}

const MAX_ALTERNATE_NAMES: usize = 100;

#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file(_: &Path, _: &Path) -> bool {
    false
}

fn replacement_survives_probe<D: Downloader + ?Sized>(
    added: &Added<'_, D>,
    scored: &ScoredTorrent,
    slots: &[(Option<u32>, Option<u32>)],
    approved: bool,
    quality: &library::FileQuality,
    was_probed: bool,
) -> (bool, i32) {
    if !added.input.subscribe.wash_cut {
        return (approved, scored.score);
    }
    // S1: deleting an owned file needs evidence about the file that replaces
    // it. Without a successful probe the quality is only the torrent title, so
    // never approve replacing an owned slot; keep the arrival as an alternate
    // (score 0) and let the probe retry decide later.
    let owns_file = slots.iter().any(|(s, e)| {
        added
            .input
            .facts
            .get(*s, *e)
            .is_some_and(|fact| fact.path.is_some())
    });
    if owns_file && scored.release.season_inferred {
        // G-H2: the season was guessed from a title without season marker;
        // that is not enough evidence to delete an owned file.
        tracing::warn!(torrent = %scored.torrent.title, "季号由解析器推断，拒绝洗版替换在位文件");
        return (false, 0);
    }
    if owns_file && !was_probed {
        tracing::warn!(
            torrent = %scored.torrent.title,
            "新文件 probe 失败，仅凭标题无法证明是升级，拒绝洗版替换并保留在位文件"
        );
        return (false, 0);
    }
    let mut release = scored.release.clone();
    release.resolution = quality.resolution.clone();
    release.codec = quality.codec.clone();
    release.hdr = quality.hdr.clone();
    let effective_filter = added.input.wash_filter.unwrap_or(added.input.filter);
    let admission = filter::admit_scored(
        vec![(scored.torrent.clone(), Some(release))],
        effective_filter,
    );
    let Some(probed) = admission.admitted.first() else {
        return (false, 0);
    };
    // Score must remain on the Filter's scale. Also reject physical downgrades
    // when no explicit ladder is configured, regardless of site/title scores.
    let safe_quality = !was_probed
        || crate::choose::collected_quality_is_safe(
            effective_filter,
            &added.input.facts,
            &probed.release,
            slots,
        );
    let replacement = approved
        && safe_quality
        && crate::slot_replacement::is_replacement_approved(
            added.input.subscribe,
            Some(effective_filter),
            &added.input.facts,
            probed,
            slots,
        );
    // Alternate rejected versions must not outrank owned facts when the ledger
    // is reloaded after restart. Fresh fill slots still keep their actual score.
    let owns_any = slots
        .iter()
        .any(|(s, e)| added.input.facts.get(*s, *e).is_some());
    (
        replacement,
        if owns_any && !replacement {
            0
        } else {
            probed.score
        },
    )
}

fn quality_for(
    probe: &dyn MediaProbe,
    src: &Path,
    release: &Release,
    control: &library::WorkControl,
) -> (library::FileQuality, QualitySource, Confidence) {
    if let Ok(quality) = probe.probe_controlled(src, control) {
        (quality, QualitySource::Probe, Confidence::High)
    } else {
        (
            library::FileQuality {
                resolution: release.resolution.clone(),
                codec: release.codec.clone(),
                hdr: release.hdr.clone(),
            },
            QualitySource::Release,
            Confidence::Low,
        )
    }
}

fn emit(bus: Option<&hooks::Bus>, step: Step) -> Result<(), SubscribeError> {
    if let Some(bus) = bus {
        bus.emit(&HookEvent { step })?;
    }
    Ok(())
}
