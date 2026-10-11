use std::path::{Path, PathBuf};

use super::controlled::{ControlledSubtitleRunner, extract_embedded_controlled};
use super::delivery::DeliveryError;
use crate::WorkControl;

/// 尝试通过 ffmpeg 提取媒体容器中的内封字幕轨到输出路径。
/// `media_path`: 视频源文件路径（本地文件或 STRM 虚拟流地址）
/// `stream_index`: 字幕轨在容器中的 stream index（如 2）
/// `codec`: 探测到的编码名称（如 subrip, ass, mov_text 等）
/// `wants_vtt`: 请求方是否期望转换为 WebVTT
/// `cache_dir`: 缓存目录
pub fn extract_embedded_subtitle(
    media_path: &Path,
    stream_index: u32,
    codec: Option<&str>,
    wants_vtt: bool,
    cache_dir: &Path,
) -> Result<(PathBuf, &'static str), String> {
    extract_embedded_subtitle_with_program(
        media_path,
        stream_index,
        codec,
        wants_vtt,
        cache_dir,
        "ffmpeg",
    )
}

/// 兼容 wrapper：委托给受控提取（默认 `WorkControl` + `ControlledSubtitleRunner`），
/// 对外签名与错误字符串保持不变。
pub fn extract_embedded_subtitle_with_program(
    media_path: &Path,
    stream_index: u32,
    codec: Option<&str>,
    wants_vtt: bool,
    cache_dir: &Path,
    program: impl AsRef<std::ffi::OsStr>,
) -> Result<(PathBuf, &'static str), String> {
    extract_embedded_controlled(
        media_path,
        stream_index,
        codec,
        wants_vtt,
        cache_dir,
        program.as_ref(),
        &WorkControl::default(),
        &ControlledSubtitleRunner,
    )
    .map_err(extraction_message)
}

/// 既有调用方看到的是裸提取错误串（不是 `DeliveryError` 的 Display 前缀）。
fn extraction_message(error: DeliveryError) -> String {
    match error {
        DeliveryError::Extraction(message) => message,
        other => other.to_string(),
    }
}
