use std::path::Path;

use super::controlled::{ControlledSubtitleRunner, deliver_subtitle_controlled};
use crate::Tracks;
use crate::WorkControl;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitlePayload {
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
}

/// 共享字幕交付能力：根据请求的 format，查找并转换/交付字幕
pub fn deliver_subtitle(
    tracks: &Tracks,
    index: u32,
    wants_vtt: bool,
) -> Result<SubtitlePayload, DeliveryError> {
    deliver_subtitle_with_source(tracks, index, wants_vtt, None, None)
}

/// 支持外挂字幕和从视频源提取内封字幕的完整交付能力。
///
/// 兼容 wrapper：委托给受控交付（默认 `WorkControl` + `ControlledSubtitleRunner`），
/// 对外签名、返回内容与错误变体保持不变。
pub fn deliver_subtitle_with_source(
    tracks: &Tracks,
    index: u32,
    wants_vtt: bool,
    source_media_path: Option<&Path>,
    cache_dir: Option<&Path>,
) -> Result<SubtitlePayload, DeliveryError> {
    deliver_subtitle_controlled(
        tracks,
        index,
        wants_vtt,
        source_media_path,
        cache_dir,
        &WorkControl::default(),
        &ControlledSubtitleRunner,
    )
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DeliveryError {
    #[error("track not found")]
    TrackNotFound,
    #[error("file missing")]
    FileMissing,
    #[error("io error: {0}")]
    Io(String),
    #[error("extraction failed: {0}")]
    Extraction(String),
}
