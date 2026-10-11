use std::path::Path;
use std::process::Command;

use crate::target::ProbeTarget;
use crate::types::{Chapter, ChapterMarker, MarkerType};

/// Classify chapter titles into Intro or Credits.
pub fn classify_chapter_title(title: &str) -> Option<MarkerType> {
    let lower = title.trim().to_lowercase();
    if is_intro_title(&lower) {
        return Some(MarkerType::IntroStart);
    }
    if is_outro_title(&lower) {
        return Some(MarkerType::CreditsStart);
    }
    None
}

fn is_intro_title(t: &str) -> bool {
    let prefixes = [
        "intro",
        "opening",
        "op",
        "theme",
        "prologue",
        "片头",
        "序幕",
        "オープニング",
        "오프닝",
    ];
    prefixes.iter().any(|&p| t == p || t.starts_with(p))
}

fn is_outro_title(t: &str) -> bool {
    let prefixes = [
        "outro",
        "ending",
        "ed",
        "credits",
        "credit",
        "preview",
        "片尾",
        "演职员",
        "幕后",
        "エンディング",
        "엔딩",
    ];
    prefixes.iter().any(|&p| t == p || t.starts_with(p))
}

/// Annotate chapters with IntroStart / IntroEnd / CreditsStart if titles match.
pub fn annotate_chapters(chapters: &[Chapter]) -> Vec<ChapterMarker> {
    chapters
        .iter()
        .map(|ch| {
            let marker = ch.title.as_deref().and_then(classify_chapter_title);
            ChapterMarker {
                start_ms: ch.start_ms,
                end_ms: ch.end_ms,
                title: ch.title.clone(),
                marker_type: marker,
                synthetic: false,
            }
        })
        .collect()
}

/// Best-effort container chapters via ffprobe (`-show_chapters`).
pub fn probe_chapters(path: &Path) -> Option<Vec<Chapter>> {
    probe_chapters_controlled(path, &crate::WorkControl::default())
}

pub fn probe_chapters_controlled(path: &Path, control: &crate::WorkControl) -> Option<Vec<Chapter>> {
    let target = ProbeTarget::from_path(path);
    let mut cmd = Command::new("ffprobe");
    cmd.args(["-v", "error", "-show_chapters", "-of", "json"]);
    target.apply_input(&mut cmd);
    let output = match control.output(&mut cmd) {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "【章节探测】ffprobe 进程启动失败"
            );
            return None;
        }
    };
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        tracing::warn!(
            path = %path.display(),
            detail = detail.chars().take(500).collect::<String>(),
            "【章节探测】ffprobe 探测失败（文件不可达 / 远程 302 后鉴权失败 / 无章节）"
        );
        return None;
    }
    #[derive(serde::Deserialize)]
    struct ChaptersOut {
        chapters: Vec<ChapterRow>,
    }
    #[derive(serde::Deserialize)]
    struct ChapterRow {
        start_time: Option<String>,
        end_time: Option<String>,
        tags: Option<serde_json::Value>,
    }
    let parsed: ChaptersOut = match serde_json::from_slice(&output.stdout) {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "【章节探测】ffprobe 输出解析失败"
            );
            return None;
        }
    };
    let chapters: Vec<Chapter> = parsed
        .chapters
        .into_iter()
        .filter_map(|row| {
            let start = row.start_time?.parse::<f64>().ok()?;
            let end = row.end_time?.parse::<f64>().ok()?;
            let title = row
                .tags
                .and_then(|t| t.get("title").and_then(|v| v.as_str()).map(String::from));
            Some(Chapter {
                start_ms: (start * 1000.0) as i64,
                end_ms: (end * 1000.0) as i64,
                title,
            })
        })
        .collect();

    Some(chapters)
}

/// 构造完整连续的剧集时间轴章节（类似 Infuse / Apple TV）：
/// 如果原本没有内嵌章节，或者只有单独的 Marker，
/// 则依据片头、片尾和视频总时长，自动切分为：
/// 1. [0 -> intro_start] (若 > 0): 序幕 / 前情提要 (Cold Open / Recap)
/// 2. [intro_start -> intro_end]: 片头 (IntroStart)
/// 3. [intro_end -> outro_start 或 总时长]: 正片
/// 4. [outro_start -> 总时长]: 片尾 (CreditsStart)
pub fn build_complete_timeline_chapters(
    existing: &[ChapterMarker],
    intro: Option<(i64, i64)>,
    outro: Option<(i64, i64)>,
    duration_ms: Option<i64>,
) -> Vec<ChapterMarker> {
    let source_chapters = source_chapters(existing);
    // 如果已经存在多段真实内嵌章节，优先保留并融合本轮 marker。
    if source_chapters.len() > 1 {
        return merge_detected_markers(source_chapters, intro, outro);
    }

    generated_timeline(source_chapters, intro, outro, duration_ms)
}

fn source_chapters(existing: &[ChapterMarker]) -> Vec<ChapterMarker> {
    if is_legacy_generated_timeline(existing) {
        return Vec::new();
    }
    existing
        .iter()
        .filter(|chapter| {
            !chapter.synthetic
                && !matches!(
                    chapter.marker_type,
                    Some(MarkerType::IntroStart | MarkerType::IntroEnd | MarkerType::CreditsStart)
                )
        })
        .cloned()
        .collect()
}

fn merge_detected_markers(
    mut chapters: Vec<ChapterMarker>,
    intro: Option<(i64, i64)>,
    outro: Option<(i64, i64)>,
) -> Vec<ChapterMarker> {
    if let Some((start_ms, end_ms)) = intro {
        chapters.push(synthetic_marker(
            start_ms,
            end_ms,
            "片头",
            MarkerType::IntroStart,
        ));
    }
    if let Some((start_ms, end_ms)) = outro {
        chapters.push(synthetic_marker(
            start_ms,
            end_ms,
            "片尾",
            MarkerType::CreditsStart,
        ));
    }
    chapters.sort_by_key(|chapter| chapter.start_ms);
    chapters
}

fn generated_timeline(
    source_chapters: Vec<ChapterMarker>,
    intro: Option<(i64, i64)>,
    outro: Option<(i64, i64)>,
    duration_ms: Option<i64>,
) -> Vec<ChapterMarker> {
    let mut segments = Vec::new();
    let total_ms = duration_ms.unwrap_or(0);

    // 1. 处理片头前（如果有冷开场/序幕/前情提要）
    let (intro_start, intro_end) = match intro {
        Some((s, e)) if e > s => (Some(s), Some(e)),
        _ => (None, None),
    };

    if let (Some(s), Some(e)) = (intro_start, intro_end) {
        if s >= 3000 {
            // 前置超过 3 秒，切分为序幕/前情
            segments.push(synthetic_chapter(0, s, "序幕", None));
        }
        segments.push(synthetic_marker(s, e, "片头", MarkerType::IntroStart));
    }

    // 确定正片起点
    let feature_start = intro_end.unwrap_or(0);

    // 2. 处理片尾
    let (outro_start, outro_end) = match outro {
        Some((s, e)) if e > s => (Some(s), Some(e)),
        Some((s, _)) if total_ms > s => (Some(s), Some(total_ms)),
        _ => (None, None),
    };

    // 3. 插入正片
    let feature_end = outro_start.unwrap_or(total_ms);
    if feature_end > feature_start || total_ms == 0 {
        let end_ms = if feature_end > feature_start {
            feature_end
        } else {
            feature_start + 1_800_000
        };
        segments.push(synthetic_chapter(feature_start, end_ms, "正片", None));
    }

    // 4. 插入片尾
    if let (Some(s), Some(e)) = (outro_start, outro_end) {
        segments.push(synthetic_marker(s, e, "片尾", MarkerType::CreditsStart));
    }

    // 若依然为空且有已有章节，回退到已有章节
    if segments.is_empty() {
        return source_chapters;
    }

    segments.sort_by_key(|c| c.start_ms);
    segments
}

fn synthetic_chapter(
    start_ms: i64,
    end_ms: i64,
    title: &str,
    marker_type: Option<MarkerType>,
) -> ChapterMarker {
    ChapterMarker {
        start_ms,
        end_ms,
        title: Some(title.into()),
        marker_type,
        synthetic: true,
    }
}

fn synthetic_marker(
    start_ms: i64,
    end_ms: i64,
    title: &str,
    marker_type: MarkerType,
) -> ChapterMarker {
    synthetic_chapter(start_ms, end_ms, title, Some(marker_type))
}

fn is_legacy_generated_timeline(chapters: &[ChapterMarker]) -> bool {
    if chapters.len() < 2 || chapters.first().is_none_or(|chapter| chapter.start_ms != 0) {
        return false;
    }
    let mut has_feature = false;
    let mut has_marker = false;
    for chapter in chapters {
        match (chapter.title.as_deref(), chapter.marker_type) {
            (Some("序幕"), None) | (Some("正片"), None) => {
                has_feature |= chapter.title.as_deref() == Some("正片");
            }
            (Some("片头"), Some(MarkerType::IntroStart))
            | (Some("片头"), Some(MarkerType::IntroEnd))
            | (Some("片尾"), Some(MarkerType::CreditsStart)) => has_marker = true,
            _ => return false,
        }
    }
    has_feature && has_marker
}
