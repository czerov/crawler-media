use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use super::{AudioTrack, SubtitleTrack, Tracks, VideoTrack};
use crate::LibraryError;

#[derive(Deserialize)]
struct ProbeOut {
    streams: Vec<ProbeStream>,
    #[serde(default)]
    format: ProbeFormat,
}

#[derive(Deserialize, Default)]
struct ProbeFormat {
    duration: Option<String>,
}

#[derive(Deserialize)]
struct ProbeStream {
    index: Option<u32>,
    codec_type: Option<String>,
    codec_name: Option<String>,
    profile: Option<String>,
    channels: Option<u32>,
    channel_layout: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    display_aspect_ratio: Option<String>,
    sample_aspect_ratio: Option<String>,
    pix_fmt: Option<String>,
    bits_per_raw_sample: Option<String>,
    bits_per_sample: Option<u32>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    field_order: Option<String>,
    bit_rate: Option<String>,
    duration: Option<String>,
    sample_rate: Option<String>,
    #[serde(default)]
    tags: ProbeTags,
    #[serde(default)]
    disposition: Disposition,
}

#[derive(Deserialize, Default)]
struct ProbeTags {
    language: Option<String>,
    title: Option<String>,
}

#[derive(Deserialize, Default)]
struct Disposition {
    default: Option<i64>,
    forced: Option<i64>,
}

/// Best-effort track list via ffprobe; a failure is reported to the caller.
pub fn probe_tracks(path: &Path) -> Result<Tracks, LibraryError> {
    probe_tracks_and_duration(path).map(|(tracks, _)| tracks)
}

/// Probe stream facts and container duration in one ffprobe request.
pub fn probe_tracks_and_duration(path: &Path) -> Result<(Tracks, Option<i64>), LibraryError> {
    probe_tracks_and_duration_with_program(path, "ffprobe")
}

fn probe_tracks_with_program(
    path: &Path,
    program: impl AsRef<std::ffi::OsStr>,
) -> Result<Tracks, LibraryError> {
    probe_tracks_and_duration_with_program(path, program).map(|(tracks, _)| tracks)
}

fn probe_tracks_and_duration_with_program(
    path: &Path,
    program: impl AsRef<std::ffi::OsStr>,
) -> Result<(Tracks, Option<i64>), LibraryError> {
    probe_tracks_controlled_program(path, program, &crate::WorkControl::default())
}

pub fn probe_tracks_and_duration_controlled(
    path: &Path,
    control: &crate::WorkControl,
) -> Result<(Tracks, Option<i64>), LibraryError> {
    probe_tracks_controlled_program(path, "ffprobe", control)
}

fn probe_tracks_controlled_program(
    path: &Path,
    program: impl AsRef<std::ffi::OsStr>,
    control: &crate::WorkControl,
) -> Result<(Tracks, Option<i64>), LibraryError> {
    let target = crate::ProbeTarget::from_path(path);
    let mut command = Command::new(program);
    command.args([
        "-v",
        "error",
        "-show_entries",
        "stream=index,codec_type,codec_name,profile,channels,channel_layout,width,height,r_frame_rate,avg_frame_rate,display_aspect_ratio,sample_aspect_ratio,pix_fmt,bits_per_raw_sample,bits_per_sample,color_space,color_transfer,color_primaries,field_order,bit_rate,duration,sample_rate:stream_tags=language,title:stream_disposition=default,forced:format=duration,bit_rate",
        "-of",
        "json",
    ]);
    target.apply_input(&mut command);
    let output = control.output(&mut command)?;
    if !output.status.success() {
        return Err(LibraryError::Probe(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    let parsed: ProbeOut = serde_json::from_slice(&output.stdout)
        .map_err(|error| LibraryError::Probe(error.to_string()))?;
    let duration_ms = parse_duration_ms(parsed.format.duration.as_deref());
    let mut tracks = tracks_from_probe(parsed);
    tracks.subtitles.extend(external_subtitle_tracks(path)?);
    Ok((tracks, duration_ms))
}

/// Sidecar subtitles whose basename belongs to this media file.
pub fn external_subtitle_tracks(path: &Path) -> Result<Vec<SubtitleTrack>, LibraryError> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let video_stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let entries = std::fs::read_dir(directory)?;
    let mut subtitle_paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let candidate = entry.path();
        let Some(extension) = candidate
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
        else {
            continue;
        };
        if !is_subtitle_extension(&extension) {
            continue;
        }
        let Some(candidate_stem) = candidate.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if belongs_to_video(candidate_stem, video_stem) {
            subtitle_paths.push((candidate, extension));
        }
    }
    let idx_stems = subtitle_paths
        .iter()
        .filter(|(_, extension)| extension == "idx")
        .filter_map(|(path, _)| path.file_stem()?.to_str().map(str::to_lowercase))
        .collect::<std::collections::HashSet<_>>();
    Ok(subtitle_paths
        .into_iter()
        .filter(|(path, extension)| {
            extension != "sub"
                || !path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .is_some_and(|stem| idx_stems.contains(&stem.to_lowercase()))
        })
        .map(|(path, extension)| external_subtitle(path, &extension, video_stem))
        .collect())
}

fn is_subtitle_extension(extension: &str) -> bool {
    [
        "srt", "ass", "ssa", "vtt", "sub", "idx", "sup", "smi", "ttml",
    ]
    .iter()
    .any(|supported| extension.eq_ignore_ascii_case(supported))
}

fn belongs_to_video(candidate_stem: &str, video_stem: &str) -> bool {
    let candidate = candidate_stem.to_lowercase();
    let video = video_stem.to_lowercase();
    candidate == video
        || candidate.strip_prefix(&video).is_some_and(|suffix| {
            suffix
                .chars()
                .next()
                .is_some_and(|character| matches!(character, '.' | ' ' | '_' | '-'))
        })
}

fn external_subtitle(path: std::path::PathBuf, extension: &str, video_stem: &str) -> SubtitleTrack {
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let suffix = stem
        .get(video_stem.len()..)
        .unwrap_or_default()
        .trim_matches(['.', ' ', '_', '-']);
    let mut language = None;
    let mut labels = Vec::new();
    let mut is_default = false;
    let mut forced = false;
    for token in suffix
        .split(['.', '_', ' '])
        .filter(|token| !token.is_empty())
    {
        match token.to_ascii_lowercase().as_str() {
            "default" => is_default = true,
            "forced" => forced = true,
            "sdh" | "hi" | "cc" => labels.push(token.to_string()),
            _ if language.is_none() && is_language_label(token) => {
                language = Some(token.to_string())
            }
            _ => labels.push(token.to_string()),
        }
    }
    SubtitleTrack {
        codec: Some(extension.to_string()),
        language,
        title: (!labels.is_empty()).then(|| labels.join(" ")),
        is_default,
        forced,
        is_external: true,
        path: Some(path.display().to_string()),
        ..Default::default()
    }
}

fn is_language_label(value: &str) -> bool {
    let parts = value.split(['-', '_']).collect::<Vec<_>>();
    matches!(parts[0].len(), 2 | 3)
        && parts[0].bytes().all(|byte| byte.is_ascii_alphabetic())
        && parts.iter().skip(1).all(|part| {
            (2..=8).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

fn tracks_from_probe(parsed: ProbeOut) -> Tracks {
    let mut tracks = Tracks::default();
    for stream in parsed.streams {
        let is_default = stream.disposition.default.unwrap_or_default() != 0;
        let forced = stream.disposition.forced.unwrap_or_default() != 0;
        match stream.codec_type.as_deref() {
            Some("video") if tracks.video.is_none() => {
                let pixel_format = stream.pix_fmt;
                let bit_depth = stream
                    .bits_per_raw_sample
                    .as_deref()
                    .and_then(|value| value.parse().ok())
                    .or(stream.bits_per_sample)
                    .or_else(|| pixel_format.as_deref().and_then(pixel_bit_depth));
                tracks.video = Some(VideoTrack {
                    stream_index: stream.index,
                    codec: stream.codec_name,
                    profile: stream.profile,
                    width: stream.width,
                    height: stream.height,
                    is_default,
                    frame_rate: stream.r_frame_rate.as_deref().and_then(parse_frame_rate),
                    average_frame_rate: stream.avg_frame_rate.as_deref().and_then(parse_frame_rate),
                    bit_rate: parse_u64(stream.bit_rate.as_deref()),
                    duration_secs: parse_f64(stream.duration.as_deref())
                        .or_else(|| parse_f64(parsed.format.duration.as_deref())),
                    aspect_ratio: stream.display_aspect_ratio,
                    sample_aspect_ratio: stream.sample_aspect_ratio,
                    pixel_format,
                    bit_depth,
                    color_space: stream.color_space,
                    color_transfer: stream.color_transfer,
                    color_primaries: stream.color_primaries,
                    field_order: stream.field_order,
                });
            }
            Some("audio") => tracks.audio.push(AudioTrack {
                stream_index: stream.index,
                codec: stream.codec_name,
                profile: stream.profile,
                channels: stream.channels,
                channel_layout: stream.channel_layout,
                language: stream.tags.language,
                title: stream.tags.title,
                sample_rate: stream.sample_rate,
                bit_rate: parse_u64(stream.bit_rate.as_deref()),
                bits_per_sample: stream.bits_per_sample,
                is_default,
                forced,
            }),
            Some("subtitle") => tracks.subtitles.push(SubtitleTrack {
                stream_index: stream.index,
                codec: stream.codec_name,
                profile: stream.profile,
                language: stream.tags.language,
                title: stream.tags.title,
                bit_rate: parse_u64(stream.bit_rate.as_deref()),
                is_default,
                forced,
                is_external: false,
                path: None,
            }),
            _ => {}
        }
    }
    tracks
}

fn parse_u64(value: Option<&str>) -> Option<u64> {
    value.and_then(|value| value.parse().ok())
}

fn parse_f64(value: Option<&str>) -> Option<f64> {
    value.and_then(|value| value.parse().ok())
}

fn parse_duration_ms(value: Option<&str>) -> Option<i64> {
    let seconds = parse_f64(value)?;
    seconds.is_finite().then_some((seconds * 1000.0) as i64)
}

fn parse_frame_rate(raw: &str) -> Option<f64> {
    let (num, den) = raw.split_once('/')?;
    let num: f64 = num.parse().ok()?;
    let den: f64 = den.parse().ok()?;
    (den != 0.0).then_some(num / den)
}

fn pixel_bit_depth(pixel_format: &str) -> Option<u32> {
    let suffix = pixel_format
        .rsplit_once('p')
        .map(|(_, suffix)| suffix)
        .unwrap_or_default();
    let digits = suffix
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    if digits.is_empty() {
        pixel_format.ends_with('p').then_some(8)
    } else {
        digits.parse().ok()
    }
}

#[cfg(test)]
mod tests;
