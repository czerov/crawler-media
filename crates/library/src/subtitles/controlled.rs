//! Controlled 内封字幕提取：唯一 staging、取消检查与 rename 发布。
//!
//! 提取命令通过 [`SubtitleRunner::output`] 执行（生产实现复用
//! `WorkControl::output` 的 kill+wait/reap 语义）；staging 用 uuid 命名，
//! 避免同 stream index 不同 format 或并发请求互相覆盖；发布用 rename，
//! 且只删除本任务自己的 staging，绝不删除其他请求已发布的 final。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::{LibraryError, ProbeTarget, Tracks, WorkControl};

use super::conversion::srt_to_vtt;
use super::delivery::{DeliveryError, SubtitlePayload};
use super::index::find_subtitle_by_index;

/// 执行 ffmpeg 提取并发布字幕的 seam；测试注入不启动真实进程的实现。
pub trait SubtitleRunner: Send + Sync {
    fn output(&self, command: &mut Command, control: &WorkControl) -> Result<Output, LibraryError>;

    /// 默认发布：rename staging 到 final（不 copy 覆盖）。
    fn publish_file(
        &self,
        staged: &Path,
        target: &Path,
        control: &WorkControl,
    ) -> Result<(), String> {
        control.publish(|| std::fs::rename(staged, target).map_err(|error| error.to_string()))
    }
}

/// 生产实现：复用 `WorkControl::output` 的取消/回收语义。
pub struct ControlledSubtitleRunner;

impl SubtitleRunner for ControlledSubtitleRunner {
    fn output(&self, command: &mut Command, control: &WorkControl) -> Result<Output, LibraryError> {
        control.output(command)
    }
}

/// 显式固定的 ffmpeg 输出格式，绝不依赖 staging 后缀猜格式。
struct SubtitleFormat {
    ext: &'static str,
    content_type: &'static str,
    codec: &'static str,
    container: &'static str,
}

fn subtitle_format(codec: Option<&str>, wants_vtt: bool) -> SubtitleFormat {
    let raw = codec.unwrap_or("srt").to_lowercase();
    if wants_vtt || raw == "webvtt" || raw == "vtt" {
        SubtitleFormat {
            ext: "vtt",
            content_type: "text/vtt; charset=utf-8",
            codec: "webvtt",
            container: "webvtt",
        }
    } else if raw == "ass" || raw == "ssa" {
        SubtitleFormat {
            ext: "ass",
            content_type: "text/x-ssa; charset=utf-8",
            codec: "copy",
            container: "ass",
        }
    } else if raw == "hdmv_pgs_subtitle" || raw == "pgs" || raw == "sup" {
        SubtitleFormat {
            ext: "sup",
            content_type: "application/octet-stream",
            codec: "copy",
            container: "sup",
        }
    } else {
        SubtitleFormat {
            ext: "srt",
            content_type: "text/plain; charset=utf-8",
            codec: "subrip",
            container: "srt",
        }
    }
}

fn cancelled() -> DeliveryError {
    DeliveryError::Extraction("subtitle extraction cancelled".into())
}

fn spawn_error(error: LibraryError) -> DeliveryError {
    if matches!(error, LibraryError::Cancelled) {
        cancelled()
    } else {
        DeliveryError::Extraction(format!("ffmpeg spawn failed: {error}"))
    }
}

/// 只删除本任务的 staging；NotFound 静默，其他错误 warn。
fn remove_staging(staged: &Path) {
    match std::fs::remove_file(staged) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(%error, path = %staged.display(), "删除字幕 staging 失败");
        }
    }
}

fn is_usable_cache(path: &Path) -> bool {
    path.is_file()
        && std::fs::metadata(path)
            .map(|meta| meta.len() > 0)
            .unwrap_or(false)
}

/// 锁内/锁外都安全的受控提取：取消时只清理自己的 staging，不触碰 final。
pub(crate) fn extract_embedded_controlled(
    media_path: &Path,
    stream_index: u32,
    codec: Option<&str>,
    wants_vtt: bool,
    cache_dir: &Path,
    program: &OsStr,
    control: &WorkControl,
    runner: &dyn SubtitleRunner,
) -> Result<(PathBuf, &'static str), DeliveryError> {
    control.check().map_err(|_| cancelled())?;
    let format = subtitle_format(codec, wants_vtt);
    let final_path = cache_dir.join(format!("sub_{stream_index}.{}", format.ext));
    if is_usable_cache(&final_path) {
        return Ok((final_path, format.content_type));
    }

    std::fs::create_dir_all(cache_dir)
        .map_err(|error| DeliveryError::Io(format!("failed to create cache dir: {error}")))?;

    let staged = cache_dir.join(format!(
        "sub_{stream_index}_{}.{}",
        uuid::Uuid::new_v4().simple(),
        format.ext
    ));

    let target = ProbeTarget::from_path(media_path);
    let mut command = Command::new(program);
    command.arg("-y");
    target.apply_ffmpeg_input(&mut command);
    command.args(["-map", &format!("0:{stream_index}")]);
    command.args(["-c:s", format.codec, "-f", format.container]);
    command.arg(&staged);
    command.stderr(Stdio::piped());

    let output = match runner.output(&mut command, control) {
        Ok(output) => output,
        Err(error) => {
            remove_staging(&staged);
            return Err(spawn_error(error));
        }
    };
    if !output.status.success() {
        remove_staging(&staged);
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail: String = detail.chars().take(500).collect();
        return Err(DeliveryError::Extraction(format!(
            "ffmpeg subtitle extraction failed: {detail}"
        )));
    }
    // 发布前再检查一次：提取完成后被取消不得写入 late cache。
    if control.check().is_err() {
        remove_staging(&staged);
        return Err(cancelled());
    }
    if let Err(error) = runner.publish_file(&staged, &final_path, control) {
        remove_staging(&staged);
        return Err(DeliveryError::Extraction(format!(
            "failed to move extracted subtitle: {error}"
        )));
    }
    Ok((final_path, format.content_type))
}

/// 受控字幕交付：外挂轨直读，内封轨经 [`extract_embedded_controlled`]。
pub fn deliver_subtitle_controlled(
    tracks: &Tracks,
    index: u32,
    wants_vtt: bool,
    source: Option<&Path>,
    cache_dir: Option<&Path>,
    control: &WorkControl,
    runner: &dyn SubtitleRunner,
) -> Result<SubtitlePayload, DeliveryError> {
    let sub = find_subtitle_by_index(tracks, index).ok_or(DeliveryError::TrackNotFound)?;
    if let Some(path) = &sub.path {
        control.check().map_err(|_| cancelled())?;
        let bytes = std::fs::read(path).map_err(|error| DeliveryError::Io(error.to_string()))?;
        let codec = sub.codec.as_deref().unwrap_or("srt").to_lowercase();
        if codec == "srt" && wants_vtt {
            let srt_text = String::from_utf8_lossy(&bytes);
            let vtt_text = srt_to_vtt(&srt_text);
            return Ok(SubtitlePayload {
                content_type: "text/vtt; charset=utf-8",
                bytes: vtt_text.into_bytes(),
            });
        }
        let content_type = match codec.as_str() {
            "vtt" => "text/vtt; charset=utf-8",
            "ass" | "ssa" => "text/x-ssa; charset=utf-8",
            _ => "text/plain; charset=utf-8",
        };
        return Ok(SubtitlePayload {
            content_type,
            bytes,
        });
    }

    let (Some(source_path), Some(cache_path)) = (source, cache_dir) else {
        return Err(DeliveryError::FileMissing);
    };
    let stream_index = sub.stream_index.unwrap_or(index);
    let (extracted_file, content_type) = extract_embedded_controlled(
        source_path,
        stream_index,
        sub.codec.as_deref(),
        wants_vtt,
        cache_path,
        OsStr::new("ffmpeg"),
        control,
        runner,
    )?;
    let bytes =
        std::fs::read(&extracted_file).map_err(|error| DeliveryError::Io(error.to_string()))?;
    Ok(SubtitlePayload {
        content_type,
        bytes,
    })
}
