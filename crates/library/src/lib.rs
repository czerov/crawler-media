mod file_transfer;
mod frame_control;
pub use frame_control::extract_frame_controlled;
pub mod fingerprint;
pub mod latest;
pub mod markers;
mod naming;
mod nfo;
mod nfo_season;
mod nfo_identity;
mod watch_identity;
pub use nfo_identity::{NfoIdentity, parse_nfo_identity};
pub use watch_identity::read_watch_identity;
mod probe_target;
mod scrape;
pub mod subtitles;
mod tracks;
mod watch;
mod work_control;
pub use work_control::WorkControl;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

pub use file_transfer::{
    hardlink_fallback_count, set_hardlink_fallback_copy, transfer_file, transfer_file_controlled,
};
pub use fingerprint::{CommonSegment, extract_audio_fingerprint, find_common_segment};
pub use latest::{WatchTier, played_last};
pub use marker::{
    Chapter, ChapterMarker, MarkerType, annotate_chapters, build_complete_timeline_chapters,
    classify_chapter_title, probe_chapters, probe_chapters_controlled,
};
pub use naming::{default_pattern, render_path, validate_pattern};
pub use nfo::{CastMember, NfoMeta, parse_nfo, read_nfo, write_nfo, write_streamdetails_into_nfo};
pub use nfo_season::{write_season_nfo, write_season_nfo_in_existing_dir};
pub use probe_target::{
    DEFAULT_PROBE_UA, ProbeTarget, active_probe_ua, read_strm_url, set_custom_probe_ua,
};
pub use scrape::{scrape_beside, scrape_directory};
pub use subtitles::{
    ControlledSubtitleRunner, DeliveryError, SubtitlePayload, SubtitleRunner, deliver_subtitle,
    deliver_subtitle_controlled, deliver_subtitle_with_source, extract_embedded_subtitle,
    find_subtitle_by_index, srt_to_vtt, subtitle_index,
};
pub use tracks::{
    AudioTrack, SubtitleTrack, Tracks, VideoTrack, classify_resolution, external_subtitle_tracks,
    probe_tracks, probe_tracks_and_duration, probe_tracks_and_duration_controlled,
};
pub use watch::{
    FileError, ScanIssue, ScanReport, ScanRootReport, ScanRootStatus, TransferredFile,
    Unidentified, VIDEO_EXTENSIONS, WatchJob, WatchKind, WatchOutcome, is_video_file, scan_watch,
    scan_watch_intake_controlled, scan_watch_reported,
};
pub use watch::{
    ScanChunk, ScanWalkBudget, ScanWalker,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferMode {
    Hardlink,
    Copy,
    Move,
}

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("Library work cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("ffprobe failed: {0}")]
    Probe(String),
    #[error("naming template: {0}")]
    Naming(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FileQuality {
    pub resolution: Option<String>,
    pub codec: Option<String>,
    pub hdr: Option<String>,
}

pub fn resolve_mode(src: &Path, dest_dir: &Path, configured: Option<TransferMode>) -> TransferMode {
    if let Some(mode) = configured {
        return mode;
    }
    if same_filesystem(src, dest_dir) {
        TransferMode::Hardlink
    } else {
        TransferMode::Copy
    }
}

pub trait MediaProbe: Send + Sync {
    fn probe(&self, path: &Path) -> Result<FileQuality, LibraryError>;
    fn probe_controlled(&self, path: &Path, control: &WorkControl) -> Result<FileQuality, LibraryError> {
        control.check()?;
        let result = self.probe(path);
        control.check()?;
        result
    }
}

pub struct Ffprobe {
    program: PathBuf,
}

impl Default for Ffprobe {
    fn default() -> Self {
        Self::with_program("ffprobe")
    }
}

impl Ffprobe {
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl MediaProbe for Ffprobe {
    fn probe(&self, path: &Path) -> Result<FileQuality, LibraryError> {
        self.probe_controlled(path, &WorkControl::default())
    }
    fn probe_controlled(&self, path: &Path, control: &WorkControl) -> Result<FileQuality, LibraryError> {
        let mut command = Command::new(&self.program);
        command
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name,width,height,color_transfer:stream_side_data",
                "-of",
                "json",
            ])
            .arg(path);
        let output = control.output(&mut command)?;
        if !output.status.success() {
            return Err(LibraryError::Probe(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        let parsed: ProbeOutput = serde_json::from_slice(&output.stdout)
            .map_err(|error| LibraryError::Probe(error.to_string()))?;
        let stream = parsed
            .streams
            .into_iter()
            .next()
            .ok_or_else(|| LibraryError::Probe("no video stream".into()))?;
        let hdr = hdr(&stream);
        Ok(FileQuality {
            resolution: resolution(stream.width, stream.height),
            codec: stream.codec_name,
            hdr,
        })
    }
}

#[derive(Deserialize)]
struct ProbeOutput {
    streams: Vec<ProbeStream>,
}

#[derive(Deserialize)]
struct ProbeStream {
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    color_transfer: Option<String>,
    #[serde(default)]
    side_data_list: Vec<ProbeSideData>,
}

#[derive(Deserialize)]
struct ProbeSideData {
    #[serde(rename = "side_data_type")]
    kind: Option<String>,
}

/// Extract a frame at `ms` into `out` (JPEG, 960px wide) via ffmpeg.
pub fn extract_frame(path: &std::path::Path, ms: i64, out: &std::path::Path) -> Result<(), String> {
    extract_frame_with_ffmpeg(path, ms, out, "ffmpeg")
}

/// 同 `extract_frame`，但 ffmpeg 程序路径可替换。测试传入假程序，避免依赖 PATH。
pub fn extract_frame_with_ffmpeg(
    path: &std::path::Path,
    ms: i64,
    out: &std::path::Path,
    ffmpeg: &str,
) -> Result<(), String> {
    let target = ProbeTarget::from_path(path);
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-y", "-ss", &format!("{:.3}", ms as f64 / 1000.0)]);
    target.apply_ffmpeg_input(&mut cmd);
    cmd.args([
        "-frames:v",
        "1",
        "-vf",
        "scale=960:-2",
        "-q:v",
        "4",
        &out.display().to_string(),
    ]);
    cmd.stderr(Stdio::piped());
    let output = match cmd.output() {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "【抓帧】ffmpeg 进程启动失败"
            );
            return Err(format!("ffmpeg spawn failed: {error}"));
        }
    };
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = detail.chars().take(500).collect::<String>();
        tracing::warn!(
            path = %path.display(),
            ms,
            detail = %detail,
            "【抓帧】ffmpeg 抓帧失败"
        );
        return Err(format!("ffmpeg frame extraction failed: {detail}"));
    }
    Ok(())
}

/// Best-effort media duration in ms via ffprobe (format=duration).
pub fn probe_duration(path: &std::path::Path) -> Option<i64> {
    let target = ProbeTarget::from_path(path);
    let mut cmd = Command::new("ffprobe");
    cmd.args([
        "-v",
        "error",
        "-show_entries",
        "format=duration",
        "-of",
        "json",
    ]);
    target.apply_input(&mut cmd);
    let output = match cmd.output() {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "【时长探测】ffprobe 进程启动失败"
            );
            return None;
        }
    };
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        tracing::warn!(
            path = %path.display(),
            detail = detail.chars().take(500).collect::<String>(),
            "【时长探测】ffprobe 探测失败"
        );
        return None;
    }
    #[derive(serde::Deserialize)]
    struct DurationOut {
        format: DurationFormat,
    }
    #[derive(serde::Deserialize)]
    struct DurationFormat {
        duration: Option<String>,
    }
    let parsed: DurationOut = match serde_json::from_slice(&output.stdout) {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "【时长探测】ffprobe 输出解析失败"
            );
            return None;
        }
    };
    let seconds: f64 = parsed.format.duration?.parse().ok()?;
    Some((seconds * 1000.0) as i64)
}

fn resolution(width: Option<u32>, height: Option<u32>) -> Option<String> {
    classify_resolution(width, height)
}

fn hdr(stream: &ProbeStream) -> Option<String> {
    let transfer = stream.color_transfer.as_deref().unwrap_or_default();
    if transfer.eq_ignore_ascii_case("smpte2084") {
        return Some("HDR10".into());
    }
    if transfer.eq_ignore_ascii_case("arib-std-b67") {
        return Some("HLG".into());
    }
    stream.side_data_list.iter().find_map(|side| {
        side.kind
            .as_deref()
            .is_some_and(|kind| kind.contains("DOVI") || kind.contains("Dolby Vision"))
            .then(|| "Dolby Vision".into())
    })
}

fn same_filesystem(src: &Path, dest_dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(src_meta) = src.metadata() else {
            return false;
        };
        let mut ancestor = dest_dir.to_path_buf();
        loop {
            if let Ok(dest_meta) = ancestor.metadata() {
                return src_meta.dev() == dest_meta.dev();
            }
            if !ancestor.pop() {
                return false;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (src, dest_dir);
        false
    }
}
