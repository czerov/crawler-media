use std::path::{Path, PathBuf};

use library::{TransferMode, resolve_mode, transfer_file_controlled};

use crate::SubscribeError;

const SUBTITLE_EXT: &[&str] = &["srt", "ass", "ssa", "sub", "idx", "sup"];

/// 视频容器扩展名白名单：只有这些文件进入视频处理流程。NFO、图片、文本等
/// 附件不算视频，绝不能进 probe / ledger / 洗版删除。
const VIDEO_EXT: &[&str] = &[
    "mkv", "mp4", "m4v", "mov", "avi", "wmv", "webm", "flv", "ts", "m2ts", "mts", "mpg", "mpeg",
    "vob", "iso",
];

pub fn is_subtitle(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            SUBTITLE_EXT
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
        .unwrap_or(false)
}

/// 视频文件（按扩展名白名单判定）。
pub fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            VIDEO_EXT
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
        .unwrap_or(false)
}

pub fn place_mapped_subtitle(
    src: &Path,
    mappings: &[crate::collection_destinations::DestinationMapping],
    configured: Option<TransferMode>,
) -> Result<(), SubscribeError> {
    place_mapped_subtitle_controlled(src, mappings, configured, &library::WorkControl::default())
}

pub fn place_mapped_subtitle_controlled(
    src: &Path,
    mappings: &[crate::collection_destinations::DestinationMapping],
    configured: Option<TransferMode>,
    control: &library::WorkControl,
) -> Result<(), SubscribeError> {
    control.check()?;
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    let lang = extract_sub_lang_suffix(stem);
    let clean = if lang.is_some() {
        stem.rsplit_once('.').map(|(s, _)| s).unwrap_or(stem)
    } else {
        stem
    };
    let mapping = match select_mapping(src, clean, mappings) {
        Some(m) => m,
        None => {
            tracing::error!(source = %src.display(), "字幕源身份无法唯一匹配视频，不猜测目的地");
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "ambiguous or missing subtitle video source",
            )
            .into());
        }
    };
    let ext = src.extension().and_then(|s| s.to_str()).unwrap_or("srt");
    let dest = build_sub_dest(&mapping.destination_path, lang, ext);
    let mode = resolve_mode(
        src,
        dest.parent().unwrap_or_else(|| Path::new(".")),
        configured,
    );
    match transfer_file_controlled(src, &dest, mode, control) {
        Ok(()) => Ok(()),
        Err(library::LibraryError::Io(err))
            if err.kind() == std::io::ErrorKind::NotFound
                && mode == TransferMode::Move
                && !src.exists()
                && dest.is_file()
                && std::fs::metadata(&dest)
                    .map(|m| m.len() > 0)
                    .unwrap_or(false) =>
        {
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}

fn select_mapping<'a>(
    src: &Path,
    clean: &str,
    mappings: &'a [crate::collection_destinations::DestinationMapping],
) -> Option<&'a crate::collection_destinations::DestinationMapping> {
    let exact: Vec<_> = mappings
        .iter()
        .filter(|m| {
            let source = Path::new(&m.source_path);
            source.parent() == src.parent()
                && source
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.eq_ignore_ascii_case(clean))
        })
        .collect();
    if exact.len() == 1 {
        return Some(exact[0]);
    }
    if !exact.is_empty() {
        return None;
    }
    let parsed = release::parse(clean);
    let candidates: Vec<_> = mappings
        .iter()
        .filter(|m| {
            let source = Path::new(&m.source_path);
            let video = release::parse(
                source
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default(),
            );
            let same_title = !parsed.title.is_empty()
                && parsed.title.eq_ignore_ascii_case(&video.title)
                && parsed.year == video.year;
            source.parent() == src.parent()
                && parsed.season.is_some()
                && parsed.episode.is_some()
                && parsed.season == video.season
                && parsed.episode == video.episode
                && same_title
        })
        .collect();
    if candidates.len() == 1 {
        Some(candidates[0])
    } else if candidates.is_empty()
        && mappings.len() == 1
        && parsed.season.is_none()
        && parsed.episode.is_none()
    {
        let candidate = &mappings[0];
        let candidate_source = Path::new(&candidate.source_path);
        let candidate_stem = candidate_source
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let candidate_clean = extract_sub_lang_suffix(candidate_stem)
            .and_then(|_| candidate_stem.rsplit_once('.').map(|(s, _)| s))
            .unwrap_or(candidate_stem);
        let video = release::parse(candidate_clean);
        let same_parent = candidate_source.parent() == src.parent();
        let same_exact_stem = clean.eq_ignore_ascii_case(candidate_clean);
        let same_title = !parsed.title.is_empty()
            && !video.title.is_empty()
            && parsed.title.eq_ignore_ascii_case(&video.title)
            && parsed.year == video.year;
        if same_parent && (same_exact_stem || same_title) {
            Some(candidate)
        } else {
            None
        }
    } else {
        None
    }
}

fn extract_sub_lang_suffix(stem: &str) -> Option<&'static str> {
    let lower = stem.to_ascii_lowercase();
    for lang in [
        ".zh-cn", ".zh-tw", ".zh", ".sc", ".tc", ".chs", ".cht", ".cn", ".en", ".eng", ".ja",
    ] {
        if lower.ends_with(lang) {
            return match lang {
                ".zh-cn" | ".chs" | ".sc" | ".cn" => Some("zh"),
                ".zh-tw" | ".cht" | ".tc" => Some("zh"),
                ".zh" => Some("zh"),
                ".en" | ".eng" => Some("en"),
                ".ja" => Some("ja"),
                _ => None,
            };
        }
    }
    None
}

fn build_sub_dest(dest_video: &Path, lang: Option<&str>, ext: &str) -> PathBuf {
    let parent = dest_video.parent().unwrap_or_else(|| Path::new("."));
    let stem = dest_video
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("video");
    if let Some(l) = lang {
        parent.join(format!("{stem}.{l}.{ext}"))
    } else {
        parent.join(format!("{stem}.{ext}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn video_whitelist() {
        assert!(is_video(Path::new("a.mkv")));
        assert!(is_video(Path::new("a.MP4")));
        assert!(is_video(Path::new("a.m2ts")));
        assert!(!is_video(Path::new("a.nfo")));
        assert!(!is_video(Path::new("poster.jpg")));
        assert!(!is_video(Path::new("README.txt")));
        assert!(!is_video(Path::new("a.srt")));
    }

    #[test]
    fn subtitle_whitelist() {
        assert!(is_subtitle(Path::new("a.srt")));
        assert!(is_subtitle(Path::new("a.ASS")));
        assert!(!is_subtitle(Path::new("a.mkv")));
        assert!(!is_subtitle(Path::new("a.nfo")));
    }
}
