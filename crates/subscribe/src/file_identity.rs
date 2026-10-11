use crate::choose::covered_slots;
use domain::{Coverage, Media, Release, Subscribe};
use std::path::Path;

fn is_generic_episode_stem(title: &str) -> bool {
    let lower = title.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return true;
    }
    let stem = std::path::Path::new(&lower)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&lower);
    let token = stem.split('.').next().unwrap_or(stem);
    let token = token.split(' ').next().unwrap_or(token);
    let token = token.split('-').next().unwrap_or(token);
    if let Some(rest) = token.strip_prefix('s') {
        if let Some((season, episode)) = rest.split_once('e') {
            return !season.is_empty()
                && !episode.is_empty()
                && season.chars().all(|c| c.is_ascii_digit())
                && episode.chars().all(|c| c.is_ascii_digit());
        }
    }
    let s = token
        .strip_prefix("ep")
        .or_else(|| token.strip_prefix('e'))
        .unwrap_or(token);
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

fn is_generic_movie_stem(title: &str) -> bool {
    let lower = title.trim().to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "" | "sample"
            | "sample mkv"
            | "trailer"
            | "preview"
            | "extra"
            | "feature"
            | "featurette"
            | "bonus"
            | "movie"
            | "video"
            | "main"
            | "better"
            | "missing"
            | "cd1"
            | "cd2"
            | "part1"
            | "part2"
            | "disc1"
            | "disc2"
    )
}

fn has_foreign_conflict(media: &Media, release: &Release) -> bool {
    if let (Some(y1), Some(y2)) = (media.year, release.year) {
        if y1 != y2 {
            return true;
        }
    }
    if !release.title.is_empty()
        && !is_generic_episode_stem(&release.title)
        && !is_generic_movie_stem(&release.title)
    {
        if crate::media_identity::media_title_matches(media, &release.title) {
            return false;
        }
        let f_title = release.title.to_ascii_lowercase();
        let m_title = media.title.to_ascii_lowercase();
        let m_clean = m_title.strip_prefix("the ").unwrap_or(&m_title).trim();
        let f_clean = f_title.strip_prefix("the ").unwrap_or(&f_title).trim();
        if !m_clean.contains(f_clean) && !f_clean.contains(m_clean) {
            return true;
        }
    }
    false
}

fn has_explicit_season_in_filename(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let upper = name.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    let len = bytes.len();
    for i in 0..len {
        let is_start = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if is_start && bytes[i] == b'S' && i + 1 < len && bytes[i + 1].is_ascii_digit() {
            return true;
        }
    }
    upper.contains("SEASON") || upper.contains("季")
}

pub fn resolve_file_release(
    subscribe: &Subscribe,
    media: &Media,
    file_path: &Path,
    file_release: &Release,
    torrent_release: &Release,
    single_file: bool,
) -> Option<Release> {
    match subscribe.coverage {
        Coverage::Tv { .. } => {
            let mut candidate = if file_release.season.is_some() || file_release.episode.is_some() {
                file_release.clone()
            } else if single_file {
                torrent_release.clone()
            } else {
                tracing::warn!(
                    path = %file_path.display(),
                    "多文件剧集中无法识别集号的文件不能继承整季 coverage"
                );
                return None;
            };
            if let Some(t_season) = torrent_release.season {
                if !has_explicit_season_in_filename(file_path) {
                    candidate.season = Some(t_season);
                }
            }
            let slots = covered_slots(subscribe, &candidate);
            if slots.is_empty() {
                return None;
            }
            if !single_file && has_foreign_conflict(media, file_release) {
                tracing::warn!(path = %file_path.display(), "多文件剧集中文件所含剧名或年份与订阅目标冲突，予以排除");
                return None;
            }
            Some(candidate)
        }
        Coverage::Movie => {
            if !single_file && has_foreign_conflict(media, file_release) {
                tracing::warn!(path = %file_path.display(), "多文件电影种子中发现片名或年份冲突的外来文件，予以排除");
                return None;
            }
            Some(torrent_release.clone())
        }
    }
}
