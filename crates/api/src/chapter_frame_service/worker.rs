//! Production ffmpeg renderer and the publication gate.
//!
//! Rendering never writes the visible sidecar: every frame goes to a unique
//! staging name. Publication rechecks the ledger source version and the Library
//! lease inside the publish critical section, so a late frame for a replaced or
//! frozen source cannot appear beside the video.

use std::path::{Path, PathBuf};
use std::process::Command;

use library::{ProbeTarget, WorkControl};

use super::{chapter_frame_path, ChapterFrameRequest, ChapterFrameRunner, ChapterFrameStaged};
use crate::library_lifecycle::LibraryWorkLease;

pub struct FfmpegChapterFrameRunner {
    ffmpeg: String,
}

impl Default for FfmpegChapterFrameRunner {
    fn default() -> Self {
        Self {
            ffmpeg: "ffmpeg".into(),
        }
    }
}

impl FfmpegChapterFrameRunner {
    pub fn new(ffmpeg: impl Into<String>) -> Self {
        Self {
            ffmpeg: ffmpeg.into(),
        }
    }
}

impl ChapterFrameRunner for FfmpegChapterFrameRunner {
    fn render(
        &self,
        request: &ChapterFrameRequest,
        control: &WorkControl,
    ) -> Result<Vec<ChapterFrameStaged>, String> {
        let mut staged = Vec::new();
        let mut failures: Vec<String> = Vec::new();
        for (index, &(start_ms, end_ms)) in request.targets.iter().enumerate() {
            if control.is_cancelled() {
                cleanup(&staged);
                return Err("cancelled".into());
            }
            let target = chapter_frame_path(&request.video, index);
            if target.is_file() {
                continue;
            }
            let midpoint = if end_ms > start_ms {
                start_ms + (end_ms - start_ms) / 2
            } else {
                start_ms
            };
            match extract_frame(&self.ffmpeg, &request.video, midpoint, &target, control) {
                Ok(item) => staged.push(item),
                Err(error) => {
                    tracing::debug!(
                        %error,
                        path = %request.video.display(),
                        index,
                        "章节场景帧提取失败"
                    );
                    failures.push(error);
                }
            }
        }
        if staged.is_empty() && !failures.is_empty() {
            return Err(failures.remove(0));
        }
        Ok(staged)
    }
}

fn extract_frame(
    ffmpeg: &str,
    video: &Path,
    midpoint: i64,
    target: &Path,
    control: &WorkControl,
) -> Result<ChapterFrameStaged, String> {
    let staged = target.with_file_name(format!(".chapter-{}.jpg", uuid::Uuid::new_v4()));
    let mut command = Command::new(ffmpeg);
    command.args(["-y", "-ss", &format!("{:.3}", midpoint as f64 / 1000.0)]);
    ProbeTarget::from_path(video).apply_ffmpeg_input(&mut command);
    command.args(["-frames:v", "1", "-vf", "scale=960:-2", "-q:v", "4"]);
    command.arg(&staged);
    let output = match control.output(&mut command) {
        Ok(output) => output,
        Err(error) => {
            remove_file(&staged);
            return Err(error.to_string());
        }
    };
    if !output.status.success() {
        remove_file(&staged);
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(ChapterFrameStaged {
        staged,
        target: target.to_path_buf(),
    })
}

/// Rename staged frames into place under the version/lease gate. Returns the
/// number published; `0` with `Ok` means the source was superseded.
pub(super) fn publish_staged(
    staged: &[ChapterFrameStaged],
    video: &Path,
    source_version: &str,
    lease: Option<&LibraryWorkLease>,
    control: &WorkControl,
) -> Result<usize, String> {
    let mut published = 0;
    for (index, item) in staged.iter().enumerate() {
        if control.is_cancelled() {
            cleanup(&staged[index..]);
            return Err("cancelled".into());
        }
        let outcome = match lease {
            Some(lease) => lease.publish(|| publish_one(item, video, source_version)),
            None => publish_one(item, video, source_version),
        };
        match outcome {
            Ok(true) => published += 1,
            Ok(false) => {
                cleanup(&staged[index..]);
                tracing::warn!(
                    path = %video.display(),
                    "章节源文件在发布前已变化，丢弃晚到场景帧"
                );
                return Ok(published);
            }
            Err(error) => {
                cleanup(&staged[index..]);
                return Err(error);
            }
        }
    }
    Ok(published)
}

fn publish_one(
    item: &ChapterFrameStaged,
    video: &Path,
    source_version: &str,
) -> Result<bool, String> {
    if crate::fingerprint_job::current_source_version(video) != source_version {
        return Ok(false);
    }
    std::fs::rename(&item.staged, &item.target).map_err(|error| error.to_string())?;
    Ok(true)
}

fn cleanup(staged: &[ChapterFrameStaged]) {
    for item in staged {
        remove_file(&item.staged);
    }
}

fn remove_file(path: &PathBuf) {
    if path.exists()
        && let Err(error) = std::fs::remove_file(path)
    {
        tracing::warn!(%error, path = %path.display(), "无法删除章节帧暂存文件");
    }
}
