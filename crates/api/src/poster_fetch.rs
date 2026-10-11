use std::path::Path;
use std::sync::Arc;

use crate::catalog::ArtworkCandidate;
use domain::Media;

use crate::http_agent;
use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;

pub trait PosterFetch: Send + Sync {
    fn get(&self, url: &str) -> Result<Vec<u8>, String>;
}

pub struct HttpPoster;

impl PosterFetch for HttpPoster {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        http_agent::call_url(url, |agent| agent.get(url).call())
            .map_err(|err| err.to_string())?
            .into_body()
            .read_to_vec()
            .map_err(|err| err.to_string())
    }
}

/// Best-effort poster for a transferred file (Emby-style):
/// 1. catalog poster URL (TMDB etc.) if reachable;
/// 2. else extract a video frame locally (network-independent fallback).
/// Writes `poster.jpg` beside the video. Any failure keeps sidecars intact.
pub fn attach_poster(state: &ApiState, media: &Media, video: &Path) -> Option<Vec<u8>> {
    if !crate::scrape_metadata::targets::video_present(video) {
        return None;
    }
    // 刮削与整理设置：写入图片总开关（mirror_images）。
    let mirror_images = state
        .store
        .lock()
        .get_scrape_config()
        .ok()
        .map(|config| config.effective.mirror_images)
        .unwrap_or(true);
    if !mirror_images {
        tracing::info!(media = %media.title, "镜像图片已关闭，跳过海报");
        return None;
    }
    if !crate::scrape_metadata::targets::video_present(video) {
        return None;
    }
    let Some(dir) = video.parent() else {
        tracing::warn!(media = %media.title, video = %video.display(), "海报落盘失败：视频没有父目录");
        return None;
    };
    let target = dir.join("poster.jpg");
    tracing::debug!(media = %media.title, video = %video.display(), target = %target.display(), "为媒体文件补充海报");

    // A user-selected or uploaded cover is authoritative. Metadata refreshes
    // and future imports must not overwrite it (Emby-style artwork lock).
    let locked = state
        .store
        .lock()
        .get_setting(&format!("artwork.{}", media.id))
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|value| value["poster_locked"].as_bool())
        .unwrap_or(false);
    if locked && target.is_file() {
        tracing::debug!(media = %media.title, path = %target.display(), "海报已锁定，保留现有文件");
        return match std::fs::read(&target) {
            Ok(bytes) => Some(bytes),
            Err(error) => {
                tracing::warn!(%error, media = %media.title, path = %target.display(), "读取已锁定海报失败");
                None
            }
        };
    }

    // Channel 1: catalog artwork. poster_mode=language picks per the language
    // priority + min-width gate; default keeps the TMDB detail poster. Both
    // paths honor poster_size so sidecars are suitable for full-screen views.
    if let Some(tmdb_id) = media.tmdb_id.as_deref() {
        let config = state.store.lock().get_scrape_config().ok();
        let size = config
            .as_ref()
            .map(|c| c.effective.poster_size.as_str())
            .unwrap_or("w780");
        let mode = config
            .as_ref()
            .map(|c| c.effective.poster_mode.as_str())
            .unwrap_or("default");
        let picked: Option<Vec<u8>> = if mode == "language" {
            match state.catalog.image_candidates(media.kind, tmdb_id) {
                Ok(candidates) => {
                    let effective = config.as_ref().map(|c| &c.effective);
                    let meta = effective.map(|e| e.primary_language()).unwrap_or("zh-CN");
                    let priority = effective
                        .map(|e| e.poster_language_priority.as_slice())
                        .unwrap_or_default();
                    let min = effective.map(|e| e.poster_min_width).unwrap_or(500);
                    let size = effective.map(|e| e.poster_size.as_str()).unwrap_or("w780");
                    match pick_candidate(&candidates.posters, priority, meta, min) {
                        Some(candidate) => {
                            fetch_artwork(state, media, "海报", &candidate.url(size))
                        }
                        None => {
                            tracing::warn!(
                                media = %media.title,
                                tmdb_id,
                                posters = candidates.posters.len(),
                                min_width = min,
                                "没有符合语言和宽度的海报候选"
                            );
                            None
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, media = %media.title, tmdb_id, "拉取海报候选失败");
                    None
                }
            }
        } else {
            match state.catalog.poster_url(media.kind, tmdb_id) {
                Ok(Some(url)) => {
                    fetch_artwork(state, media, "海报", &poster_url_at_size(&url, size))
                }
                Ok(None) => {
                    tracing::warn!(media = %media.title, tmdb_id, "目录没有海报地址");
                    None
                }
                Err(error) => {
                    tracing::warn!(%error, media = %media.title, tmdb_id, "查询海报地址失败");
                    None
                }
            }
        };
        if let Some(bytes) = picked {
            if !crate::scrape_metadata::targets::video_present(video) {
                return None;
            }
            return write_artwork(media, &target, "海报", &bytes);
        }
    }

    // Channel 2: local video frame → 2:3 poster (no network needed).
    // 对于 STRM 虚拟流媒体文件，跳过本地强制抓帧（避免 ffmpeg 读纯文本报错），
    // 且避免对远程挂载发起大流量视频流拉取
    let is_strm = video
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("strm"));
    if !is_strm {
        let generate_allowed = {
            let store = state.store.lock();
            if let Ok(libraries) = store.list_libraries() {
                libraries
                    .into_iter()
                    .filter(|lib| lib.root_paths.iter().any(|r| video.starts_with(r)))
                    .max_by_key(|lib| {
                        lib.root_paths
                            .iter()
                            .filter(|r| video.starts_with(r))
                            .map(|r| r.components().count())
                            .max()
                            .unwrap_or(0)
                    })
                    .map(|lib| lib.generate_thumbnails)
                    .unwrap_or(true)
            } else {
                true
            }
        };
        if generate_allowed {
            match frame_poster(video) {
                Ok(bytes) if !bytes.is_empty() => {
                    return write_artwork(media, &target, "海报", &bytes);
                }
                Ok(_) => {
                    tracing::warn!(media = %media.title, video = %video.display(), "视频截帧海报为空");
                }
                Err(error) => {
                    tracing::warn!(%error, media = %media.title, video = %video.display(), "视频截帧海报失败");
                }
            }
        }
    }
    tracing::warn!(
        media = %media.title,
        video = %video.display(),
        target = %target.display(),
        "海报未写入"
    );
    None
}

fn fetch_artwork(state: &ApiState, media: &Media, kind: &str, url: &str) -> Option<Vec<u8>> {
    match state.poster_fetch.get(url) {
        Ok(bytes) if !bytes.is_empty() => Some(bytes),
        Ok(_) => {
            tracing::warn!(media = %media.title, url, "{kind}下载结果为空");
            None
        }
        Err(error) => {
            tracing::warn!(%error, media = %media.title, url, "{kind}下载失败");
            None
        }
    }
}

fn write_artwork(media: &Media, target: &Path, kind: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    match std::fs::write(target, bytes) {
        Ok(()) => {
            tracing::info!(
                media = %media.title,
                path = %target.display(),
                bytes = bytes.len(),
                "{kind}已写入"
            );
            Some(bytes.to_vec())
        }
        Err(error) => {
            if error.kind() == std::io::ErrorKind::NotFound {
                tracing::debug!(path = %target.display(), "目标目录不存在，跳过{kind}写入");
            } else {
                tracing::warn!(%error, media = %media.title, path = %target.display(), "{kind}写入失败");
            }
            None
        }
    }
}

fn poster_url_at_size(url: &str, size: &str) -> String {
    url.replace("/t/p/w342/", &format!("/t/p/{size}/"))
}

/// Extract a 2:3 poster (342x513) from a video frame via ffmpeg.
pub fn frame_poster(video: &Path) -> Result<Vec<u8>, String> {
    use std::process::Command;
    let out = tempfile_posters_dir()?.join(format!(
        "frame-{}-{}.jpg",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let status = Command::new("ffmpeg")
        .args([
            "-y", "-ss", "60", "-i", &video.display().to_string(),
            "-frames:v", "1",
            "-vf", "scale=342:513:force_original_aspect_ratio=decrease,pad=342:513:(ow-iw)/2:(oh-ih)/2:black",
            "-q:v", "2", &out.display().to_string(),
        ])
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("ffmpeg frame extraction failed".into());
    }
    let bytes = std::fs::read(&out).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&out);
    Ok(bytes)
}

fn tempfile_posters_dir() -> Result<std::path::PathBuf, String> {
    let dir = std::env::temp_dir().join("crawler-media-posters");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

pub fn poster_fetch(state: &ApiState) -> Arc<dyn PosterFetch> {
    state.poster_fetch.clone()
}

/// Pick an artwork candidate by language-priority tiers + min-width gate.
/// Priority items: "" = 无文字 (null language), "meta" = metadata primary
/// language, otherwise a language code (subtag match). Falls back to any
/// candidate above the gate, then the first candidate.
pub fn pick_candidate<'a>(
    candidates: &'a [ArtworkCandidate],
    priority: &[String],
    meta_language: &str,
    min_width: u32,
) -> Option<&'a ArtworkCandidate> {
    let meta = lang_subtag(meta_language);
    for item in priority {
        let want = match item.trim() {
            "" => Some(None),
            "meta" => Some(Some(meta.clone())),
            lang => Some(Some(lang_subtag(lang))),
        };
        let Some(want) = want else { continue };
        if let Some(candidate) = candidates
            .iter()
            .find(|c| c.width >= min_width && lang_matches(&c.language, &want))
        {
            return Some(candidate);
        }
    }
    candidates
        .iter()
        .find(|c| c.width >= min_width)
        .or_else(|| candidates.first())
}

/// Best-effort backdrop (fanart.jpg) beside a file, using the backdrop
/// language priority + min-width gate. Any failure keeps sidecars intact.
pub fn attach_backdrop(state: &ApiState, media: &Media, video: &Path) -> Option<Vec<u8>> {
    attach_backdrop_opts(state, media, video, false)
}

pub fn force_attach_backdrop(state: &ApiState, media: &Media, video: &Path) -> Option<Vec<u8>> {
    attach_backdrop_opts(state, media, video, true)
}

fn attach_backdrop_opts(
    state: &ApiState,
    media: &Media,
    video: &Path,
    force: bool,
) -> Option<Vec<u8>> {
    let Some(config) = state.store.lock().get_scrape_config().ok() else {
        tracing::warn!(media = %media.title, "读取刮削配置失败，跳过背景图");
        return None;
    };
    if !config.effective.mirror_images {
        tracing::info!(media = %media.title, "镜像图片已关闭，跳过背景图");
        return None;
    }
    if !crate::scrape_metadata::targets::video_present(video) {
        return None;
    }
    let Some(dir) = video.parent() else {
        tracing::warn!(media = %media.title, video = %video.display(), "背景图落盘失败：视频没有父目录");
        return None;
    };
    let show_dir = if dir
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().starts_with("season"))
    {
        dir.parent().unwrap_or(dir)
    } else {
        dir
    };
    let target = show_dir.join("fanart.jpg");
    if !force && target.is_file() {
        tracing::debug!(media = %media.title, path = %target.display(), "背景图已存在，跳过下载");
        return match std::fs::read(&target) {
            Ok(bytes) => Some(bytes),
            Err(error) => {
                tracing::warn!(%error, media = %media.title, path = %target.display(), "读取已有背景图失败");
                None
            }
        };
    }
    let Some(tmdb_id) = media.tmdb_id.as_deref() else {
        tracing::warn!(media = %media.title, "没有 TMDB ID，跳过背景图");
        return None;
    };
    let candidates = match state.catalog.image_candidates(media.kind, tmdb_id) {
        Ok(candidates) => candidates,
        Err(error) => {
            tracing::warn!(%error, media = %media.title, tmdb_id, "拉取背景图候选失败");
            return None;
        }
    };
    if candidates.backdrops.is_empty() {
        tracing::warn!(media = %media.title, tmdb_id, "目录没有背景图候选");
        return None;
    }
    let Some(picked) = pick_candidate(
        &candidates.backdrops,
        &config.effective.backdrop_language_priority,
        config.effective.primary_language(),
        config.effective.backdrop_min_width,
    ) else {
        tracing::warn!(
            media = %media.title,
            tmdb_id,
            backdrops = candidates.backdrops.len(),
            min_width = config.effective.backdrop_min_width,
            "没有符合语言和宽度的背景图候选"
        );
        return None;
    };
    let url = picked.url(&config.effective.backdrop_size);
    let Some(bytes) = fetch_artwork(state, media, "背景图", &url) else {
        return None;
    };
    if !crate::scrape_metadata::targets::video_present(video) {
        return None;
    }
    write_artwork(media, &target, "背景图", &bytes)
}

fn lang_subtag(language: &str) -> String {
    language
        .split('-')
        .next()
        .unwrap_or(language)
        .to_lowercase()
}

fn lang_matches(candidate: &Option<String>, want: &Option<String>) -> bool {
    match (candidate, want) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        (None, None) => true,
        _ => false,
    }
}
