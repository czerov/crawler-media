//! ffprobe stream facts cached by the API layer for media detail and playback.

mod probe;

use serde::{Deserialize, Serialize};

pub use probe::{external_subtitle_tracks, probe_tracks, probe_tracks_and_duration, probe_tracks_and_duration_controlled};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioTrack {
    pub stream_index: Option<u32>,
    pub codec: Option<String>,
    pub profile: Option<String>,
    pub channels: Option<u32>,
    pub channel_layout: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    /// ffprobe's sample_rate value, normally a decimal string such as "44100".
    pub sample_rate: Option<String>,
    pub bit_rate: Option<u64>,
    pub bits_per_sample: Option<u32>,
    pub is_default: bool,
    pub forced: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubtitleTrack {
    pub stream_index: Option<u32>,
    pub codec: Option<String>,
    pub profile: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub bit_rate: Option<u64>,
    pub is_default: bool,
    pub forced: bool,
    pub is_external: bool,
    pub path: Option<String>,
}

/// Main video stream facts, including the values needed to describe resolution,
/// rate, color, and encoding profile in Jellyfin's MediaStreams response.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoTrack {
    pub stream_index: Option<u32>,
    pub codec: Option<String>,
    pub profile: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub is_default: bool,
    pub frame_rate: Option<f64>,
    pub average_frame_rate: Option<f64>,
    pub bit_rate: Option<u64>,
    pub duration_secs: Option<f64>,
    pub aspect_ratio: Option<String>,
    pub sample_aspect_ratio: Option<String>,
    pub pixel_format: Option<String>,
    pub bit_depth: Option<u32>,
    pub color_space: Option<String>,
    pub color_transfer: Option<String>,
    pub color_primaries: Option<String>,
    pub field_order: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tracks {
    pub video: Option<VideoTrack>,
    pub audio: Vec<AudioTrack>,
    pub subtitles: Vec<SubtitleTrack>,
}

/// Classify video resolution from frame dimensions (e.g. 2160p, 1080p, 720p).
pub fn classify_resolution(width: Option<u32>, height: Option<u32>) -> Option<String> {
    let height = height?;
    Some(match (width.unwrap_or_default(), height) {
        (w, h) if w >= 3800 || h >= 2100 => "2160p".into(),
        (_, h) if h >= 1000 => "1080p".into(),
        (_, h) if h >= 700 => "720p".into(),
        (_, h) => format!("{h}p"),
    })
}

impl VideoTrack {
    /// Detect resolution tier for this video track.
    pub fn resolution(&self) -> Option<String> {
        classify_resolution(self.width, self.height)
    }

    /// Detect HDR format from color transfer or profile/side-data tags.
    pub fn hdr(&self) -> Option<String> {
        let transfer = self.color_transfer.as_deref().unwrap_or_default();
        if transfer.eq_ignore_ascii_case("smpte2084") {
            return Some("HDR10".into());
        }
        if transfer.eq_ignore_ascii_case("arib-std-b67") {
            return Some("HLG".into());
        }
        None
    }
}
