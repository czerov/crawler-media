use std::fs;
use std::path::PathBuf;

use domain::{Confidence, Media, MediaKind, Release};

use crate::scrape::scrape_beside;
use crate::{LibraryError, TransferMode, resolve_mode};
pub mod report;
pub use report::{ScanIssue, ScanReport, ScanRootReport, ScanRootStatus};
pub mod walker;
pub use walker::{ScanChunk, ScanWalkBudget, ScanWalker};

pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "m4v", "mov", "avi", "wmv", "webm", "flv", "ts", "m2ts", "mts", "mpg", "mpeg",
    "vob", "iso", "strm",
];

pub fn is_video_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            VIDEO_EXTENSIONS
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchKind {
    Intake,
    InPlace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchJob {
    pub kind: WatchKind,
    pub path: PathBuf,
    pub library_root: PathBuf,
    /// B11: 可选的电视剧媒体库根路径。若设置，intake 解析出的剧集视频文件转移到该路径。
    pub tv_library_root: Option<PathBuf>,
    /// 写 NFO 等侧车文件（总闸 + mirror_nfo 的合取）。
    pub scrape: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unidentified {
    pub path: PathBuf,
    pub confidence: Confidence,
}

/// 一个已转移（或已确认存在）的文件，连同 scan 阶段识别的 Release。
/// `identified_release` 携带 NFO/父目录 fallback 后的解析结果；
/// `None` 表示 intake 路径中文件名本身置信度足够，调用方可按 basename 解析。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferredFile {
    pub path: PathBuf,
    /// scan 阶段识别结果；None = 文件名即可信，调用方自行解析 basename。
    pub identified_release: Option<Release>,
    pub nfo_identity: Option<crate::NfoIdentity>,
}

impl PartialEq<PathBuf> for TransferredFile {
    fn eq(&self, other: &PathBuf) -> bool {
        &self.path == other
    }
}

impl PartialEq<TransferredFile> for PathBuf {
    fn eq(&self, other: &TransferredFile) -> bool {
        self == &other.path
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileError {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Default)]
pub struct WatchOutcome {
    pub transferred: Vec<TransferredFile>,
    pub unidentified: Vec<Unidentified>,
    pub errors: Vec<FileError>,
}

pub fn scan_watch(job: &WatchJob, media: &Media) -> Result<WatchOutcome, LibraryError> {
    let (outcome, _) = scan_watch_reported(job, media)?;
    Ok(outcome)
}

pub fn scan_watch_reported(
    job: &WatchJob,
    media: &Media,
) -> Result<(WatchOutcome, report::ScanRootReport), LibraryError> {
    match job.kind {
        WatchKind::InPlace => in_place_reported(job, media.kind),
        WatchKind::Intake => {
            let outcome = intake(job, media)?;
            let root_report = report::ScanRootReport {
                root: job.path.clone(),
                status: report::ScanRootStatus::Completed,
                visited: outcome.transferred.len(),
                skipped: 0,
                error_count: 0,
                issues: vec![],
            };
            Ok((outcome, root_report))
        }
    }
}

fn in_place_reported(job: &WatchJob, kind: MediaKind) -> Result<(WatchOutcome, report::ScanRootReport), LibraryError> {
    let mut outcome = WatchOutcome::default();
    let mut report = report::ScanRootReport {
        root: job.path.clone(),
        status: report::ScanRootStatus::Completed,
        visited: 0,
        skipped: 0,
        error_count: 0,
        issues: vec![],
    };
    if !job.path.exists() {
        report.status = report::ScanRootStatus::Unreachable;
        return Ok((outcome, report));
    }
    walk_in_place_reported(&job.path, &job.library_root, kind, job.scrape, &mut outcome, &mut report)?;
    if report.error_count > 0 && report.status != report::ScanRootStatus::Unreachable {
        report.status = report::ScanRootStatus::Partial;
    }
    Ok((outcome, report))
}

fn walk_in_place_reported(
    dir: &std::path::Path,
    root: &std::path::Path,
    kind: MediaKind,
    scrape: bool,
    outcome: &mut WatchOutcome,
    report: &mut report::ScanRootReport,
) -> Result<(), LibraryError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            report.error_count += 1;
            report.issues.push(report::ScanIssue {
                path: dir.to_path_buf(),
                kind: e.kind(),
            });
            report.status = report::ScanRootStatus::Partial;
            return Ok(());
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                report.error_count += 1;
                report.issues.push(report::ScanIssue {
                    path: dir.to_path_buf(),
                    kind: e.kind(),
                });
                continue;
            }
        };
        let path = entry.path();
        report.visited += 1;
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) => {
                report.error_count += 1;
                report.issues.push(report::ScanIssue {
                    path,
                    kind: e.kind(),
                });
                continue;
            }
        };
        if meta.file_type().is_symlink() {
            report.skipped += 1;
            continue;
        }
        if meta.is_dir() {
            walk_in_place_reported(&path, root, kind, scrape, outcome, report)?;
            continue;
        }
        if !is_video_file(&path) {
            report.skipped += 1;
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let mut parsed = release::parse(name);
        if let Some(parent) = path.parent() {
            enrich_from_ancestors(&mut parsed, parent, root);
        }
        let nfo_identity = identify_local(&mut parsed, &path, root, kind);
        if parsed.confidence == Confidence::Low {
            continue;
        }
        let media = Media {
            id: domain::MediaId::new(),
            kind: if parsed.season.is_some() {
                MediaKind::Tv
            } else {
                MediaKind::Movie
            },
            title: parsed.title.clone(),
            year: parsed.year,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        if scrape && media.tmdb_id.is_some() {
            scrape_beside(&path, &media, true, None)?;
        }
        outcome.transferred.push(TransferredFile {
            path,
            identified_release: Some(parsed),
            nfo_identity,
        });
    }
    Ok(())
}

/// 文件名已经能确定剧名时，仍从上层目录补年份和别名。
/// 文件名本身是 Low 时，才用父目录标题替换；NFO 在单独的有界步骤处理。
pub(crate) fn enrich_from_ancestors(parsed: &mut Release, start: &std::path::Path, root: &std::path::Path) {
    let mut current = Some(start);
    let mut depth = 0;
    while let Some(dir) = current {
        if depth >= 4 {
            break;
        }
        depth += 1;
        if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
            let parent_parsed = release::parse(name);
            if parsed.confidence == Confidence::Low && parent_parsed.confidence != Confidence::Low {
                parsed.title = parent_parsed.title;
                parsed.year = parsed.year.or(parent_parsed.year);
                parsed.season = parsed.season.or(parent_parsed.season);
                parsed.episode = parsed.episode.or(parent_parsed.episode);
                parsed.confidence = parent_parsed.confidence;
                return;
            }
            if parsed.year.is_none() {
                parsed.year = parent_parsed.year;
            }
        }
        if dir == root {
            break;
        }
        current = dir.parent();
    }
}

pub(crate) fn identify_local(
    parsed: &mut Release,
    path: &std::path::Path,
    root: &std::path::Path,
    kind: MediaKind,
) -> Option<crate::NfoIdentity> {
    if let Err(error) = crate::watch_identity::enrich_episode(parsed, path, root) {
        tracing::warn!(path = %path.display(), error = %error, "Unable to read episode NFO; using filename");
    }
    let identity = match crate::read_watch_identity(path, root, kind) {
        Ok(identity) => identity,
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error, "Unable to read work NFO; using filename");
            None
        }
    };
    if let Some(identity) = &identity {
        if let Some(title) = &identity.title {
            parsed.title = title.clone();
            parsed.confidence = Confidence::High;
        }
        parsed.year = identity.year;
    }
    identity
}


fn intake(job: &WatchJob, media: &Media) -> Result<WatchOutcome, LibraryError> {
    let mut outcome = WatchOutcome::default();
    walk_intake(&job.path, job, media, &mut outcome, None)?;
    Ok(outcome)
}

pub fn scan_watch_intake_controlled(
    job: &WatchJob, media: &Media, movie: &crate::WorkControl, tv: Option<&crate::WorkControl>,
) -> Result<WatchOutcome, LibraryError> {
    movie.check()?;
    let mut outcome = WatchOutcome::default();
    walk_intake(&job.path, job, media, &mut outcome, Some((movie, tv)))?;
    Ok(outcome)
}

fn same_entity(source: &std::path::Path, destination: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        source
            .metadata()
            .ok()
            .zip(destination.metadata().ok())
            .is_some_and(|(src, dst)| src.dev() == dst.dev() && src.ino() == dst.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = (source, destination);
        false
    }
}

fn walk_intake(
    current_dir: &std::path::Path,
    job: &WatchJob,
    media: &Media,
    outcome: &mut WatchOutcome,
    controls: Option<(&crate::WorkControl, Option<&crate::WorkControl>)>,
) -> Result<(), LibraryError> {
    for entry in fs::read_dir(current_dir)? {
        if let Some((movie, tv)) = controls {
            movie.check()?;
            if let Some(tv) = tv { tv.check()?; }
        }
        let path = entry?.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            walk_intake(&path, job, media, outcome, controls)?;
            continue;
        }
        if !meta.is_file() || !is_video_file(&path) {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let mut parsed = release::parse(name);
        if let Some(parent) = path.parent() {
            enrich_from_ancestors(&mut parsed, parent, &job.path);
        }
        let nfo_identity = identify_local(&mut parsed, &path, &job.path, media.kind);
        if parsed.confidence == Confidence::Low {
            outcome.unidentified.push(Unidentified {
                path,
                confidence: Confidence::Low,
            });
            continue;
        }
        let is_tv = parsed.season.is_some() || parsed.episode.is_some();
        let control = match controls {
            Some((_, None)) if is_tv => return Err(LibraryError::Naming("TV Library is not configured".into())),
            Some((_, Some(tv))) if is_tv => tv.clone(),
            Some((movie, _)) => movie.clone(),
            None => crate::WorkControl::default(),
        };
        control.check()?;
        let target_root = if is_tv {
            job.tv_library_root.as_ref().unwrap_or(&job.library_root)
        } else {
            &job.library_root
        };

        // 如果文件位于子目录中（例如 watch/Show Title/S01E01.mp4 或 watch/Show Title/Season 1/S01E01.mp4），
        // 在目标库中保持相同的相对层级结构，避免把各集直接平铺到电视剧库根目录下
        let rel_path = path.strip_prefix(&job.path).unwrap_or(&path);
        let dest = target_root.join(rel_path);

        if dest.exists() {
            if same_entity(&path, &dest) {
                outcome.transferred.push(TransferredFile {
                    path: dest, identified_release: Some(parsed), nfo_identity,
                });
            } else {
                tracing::error!(src=%path.display(), dest=%dest.display(), "Watch intake 目标冲突，拒绝覆盖");
                outcome.errors.push(FileError { path: path.clone(),
                    error: format!("watch intake destination already exists: {}", dest.display()) });
            }
            continue;
        }
        let mode = resolve_mode(&path, target_root, Some(TransferMode::Hardlink));
        if let Err(err) = crate::transfer_file_controlled(&path, &dest, mode, &control) {
            tracing::error!(
                src = %path.display(),
                dest = %dest.display(),
                error = %err,
                "Watch intake transfer file failed"
            );
            outcome.errors.push(FileError {
                path: path.clone(),
                error: err.to_string(),
            });
            continue;
        }
        if job.scrape {
            if let Err(err) = control.publish(|| scrape_beside(&dest, media, true, None).map_err(|e| e.to_string())) {
                tracing::warn!(
                    dest = %dest.display(),
                    error = %err,
                    "Watch intake scrape beside failed"
                );
            }
        }
        outcome.transferred.push(TransferredFile {
            path: dest,
            identified_release: Some(parsed),
            nfo_identity,
        });
    }
    Ok(())
}
