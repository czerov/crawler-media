//! Resolver for episode/movie markers without holding the global Store lock
//! across ffprobe, HTTP, or audio fingerprint work.

use std::collections::HashSet;
use std::path::Path;

use domain::{LedgerRow, Media, MediaKind};
use marker::{ChapterMarker, MarkerType, annotate_chapters, probe_chapters};

use crate::management::ApiState;
use crate::store::StoredMediaMarker;
use crate::theintrodb::TheIntroDbClient;

#[derive(Clone)]
struct ResolveSnapshot {
    row: LedgerRow,
    media: Media,
    /// 章节解析缓存：None = 从未探测；Some(任意) = 已有结果（含空结果），
    /// 命中后直接返回，避免无章节媒体每次播放都重跑 ffprobe / 声纹 / 云端。
    cached: Option<Vec<ChapterMarker>>,
    existing_marker: Option<StoredMediaMarker>,
    detect_intros: bool,
    season_rows: Vec<LedgerRow>,
    rows_with_markers: HashSet<(u32, u32)>,
    theintrodb: Option<TheIntroDbClient>,
}

struct ResolveOutput {
    chapters: Vec<ChapterMarker>,
    markers: Vec<StoredMediaMarker>,
}

/// Clear persisted results for this unit, then resolve it again.
pub async fn force_refresh_item_chapters(
    state: &ApiState,
    row: &LedgerRow,
    media: &Media,
    theintrodb: Option<TheIntroDbClient>,
) -> Vec<ChapterMarker> {
    {
        let store = state.store.lock();
        let _ = store.clear_cached_chapters(&row.id.to_string());
        let locked = store
            .get_media_marker(media.id, row.season, row.episode)
            .ok()
            .flatten()
            .is_some_and(|marker| marker.locked);
        if !locked {
            let _ = store.delete_media_marker(media.id, row.season, row.episode);
        }
    }
    resolve_item_chapters(state, row, media, theintrodb).await
}

/// Return persisted markers when source detection is disabled, but do not run
/// embedded chapter or TheIntroDB lookup in this synchronous resolver. Audio
/// fingerprint extraction runs independently in the background probe queue.
pub async fn resolve_item_chapters(
    state: &ApiState,
    row: &LedgerRow,
    media: &Media,
    theintrodb: Option<TheIntroDbClient>,
) -> Vec<ChapterMarker> {
    let _lease = match state.acquire_path_work(std::path::Path::new(&row.path), media.kind) {
        Ok(lease) => lease,
        Err(_) => return Vec::new(),
    };
    let snapshot = snapshot(state, row, media, theintrodb);
    // 存在缓存时：若缓存仅包含 1 个孤立的片头卡片，或为空，但已有/同季有 marker，
    // 则动态构造分段章节更新缓存并返回，确保前端展示 [0:00 片头] 和 [1:44 正片] 两个完整段落！
    if let Some(cached) = snapshot.cached.clone() {
        let is_only_intro =
            cached.len() == 1 && cached[0].marker_type == Some(MarkerType::IntroStart);
        if !cached.is_empty() && !is_only_intro {
            return cached;
        }
        let store = state.store.lock();
        if crate::fingerprint_job::source::ensure_source_available(&store, row).is_err() { return Vec::new(); }
        // A read may reconstruct chapters from this episode's verified marker,
        // but cannot promote a neighboring episode's candidate into target facts.
        let marker_candidate = store
            .get_media_marker(media.id, row.season, row.episode)
            .ok()
            .flatten();

        if let Some(latest) = marker_candidate {
            let mut chapters = Vec::new();
            append_marker_chapters(&mut chapters, Some(&latest));
            if !chapters.is_empty() {
                chapters.sort_by_key(|c| c.start_ms);
                let _ = store.put_cached_chapters(&row.id.to_string(), &chapters);
                return chapters;
            }
        }
        if !is_only_intro {
            return cached;
        }
    }
    let ledger_id = row.id.to_string();
    let output = tokio::task::spawn_blocking(move || resolve_without_store(snapshot))
        .await
        .unwrap_or_else(|error| {
            tracing::error!(%error, "章节标记解析任务失败");
            ResolveOutput {
                chapters: Vec::new(),
                markers: Vec::new(),
            }
        });
    let mut chapters = output.chapters;
    {
        let store = state.store.lock();
        if crate::fingerprint_job::source::ensure_source_available(&store, row).is_err() { return Vec::new(); }
        for marker in &output.markers {
            let _ = store.put_media_marker(marker);
        }
        // 竞态防护：解析开始后后台探测任务可能已写入片头标记；写缓存前
        // 重新读最新 marker 并进章节，避免把解析开始时快照里的旧空结果
        // 重新落盘覆盖掉新标记（否则章节缓存会永久停留在旧结果）。
        if let Some(latest) = store
            .get_media_marker(media.id, row.season, row.episode)
            .ok()
            .flatten()
        {
            append_marker_chapters(&mut chapters, Some(&latest));
            chapters.sort_by_key(|chapter| chapter.start_ms);
        }
        // 无条件缓存章节结果（含空），让"已检测但无章节"也能命中缓存。
        let _ = store.put_cached_chapters(&ledger_id, &chapters);
    }
    chapters
}

fn snapshot(
    state: &ApiState,
    row: &LedgerRow,
    media: &Media,
    theintrodb: Option<TheIntroDbClient>,
) -> ResolveSnapshot {
    let store = state.store.lock();
    let cached = store
        .get_cached_chapters(&row.id.to_string())
        .ok()
        .flatten();
    let existing_marker = store
        .get_media_marker(media.id, row.season, row.episode)
        .ok()
        .flatten();
    let library = crate::http::playback::library_id_for(&store, media, &[row])
        .and_then(|id| store.get_library(&id).ok().flatten());
    let season = row.season.unwrap_or(1);
    let season_rows: Vec<_> = store
        .list_ledger()
        .unwrap_or_default()
        .into_iter()
        .filter(|candidate| {
            candidate.media_id == media.id && candidate.season.unwrap_or(1) == season
        })
        .collect();
    let rows_with_markers = season_rows
        .iter()
        .filter(|candidate| {
            store
                .get_media_marker(media.id, candidate.season, candidate.episode)
                .ok()
                .flatten()
                // 只要有任意标记（片头或片尾）就算「已处理」：声纹回填只产出
                // 片头字段，若某集仅存片尾标记也放进来，会把它整行覆盖掉。
                .is_some_and(|marker| {
                    marker.intro_start_ms.is_some() || marker.outro_start_ms.is_some()
                })
        })
        .map(|candidate| {
            (
                candidate.season.unwrap_or(1),
                candidate.episode.unwrap_or(1),
            )
        })
        .collect();
    let detect_intros = library.as_ref().is_none_or(|library| library.detect_intros);
    // 同步解析链路不做声纹比对（无论 STRM 还是本地文件）：远程/本地音频
    // 提取都是重操作，不能阻塞详情页/播放器请求。声纹统一由**后台异步探测
    // 任务**（ProbeManager）执行——一次拉流同时采 streamdetails 与指纹，
    // 同季 ≥2 集后自动比对识别片头。这里同步链路只走内嵌章节 + TheIntroDB。
    // 仅构造快照，杜绝在每次查询缓存时都刷屏输出「开始解析」日志。
    ResolveSnapshot {
        row: row.clone(),
        media: media.clone(),
        cached,
        existing_marker,
        detect_intros,
        season_rows,
        rows_with_markers,
        theintrodb,
    }
}

fn resolve_without_store(snapshot: ResolveSnapshot) -> ResolveOutput {
    let mut markers = Vec::new();
    let mut chapters = Vec::new();
    let mut marker = snapshot.existing_marker.clone();
    tracing::info!(
        media = %snapshot.media.title,
        season = snapshot.row.season.unwrap_or(1),
        episode = snapshot.row.episode.unwrap_or(1),
        path = %snapshot.row.path,
        "【片头片尾】缓存未命中，后台异步执行单集章节探测"
    );
    if !snapshot.detect_intros {
        tracing::warn!(
            media = %snapshot.media.title,
            path = %snapshot.row.path,
            "【片头片尾】跳过检测：该媒体库未开启「识别片头片尾」，仅返回已存标记"
        );
        append_marker_chapters(&mut chapters, marker.as_ref());
        return ResolveOutput { chapters, markers };
    }

    // STRM 同步解析不做内嵌章节探测：ffprobe 对远程 URL 每次最多等 30s，
    // 会拖慢 PlaybackInfo/详情页请求。STRM 的片头片尾走 TheIntroDB 云端
    // 与后台探测任务的声纹（不阻塞请求）。
    let is_strm = std::path::Path::new(&snapshot.row.path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("strm"));
    if is_strm {
        tracing::info!(
            media = %snapshot.media.title,
            path = %snapshot.row.path,
            "【片头片尾】STRM 同步解析跳过 ffprobe 章节探测（避免阻塞请求），走 TheIntroDB / 后台声纹"
        );
        if marker.is_none() && snapshot.media.kind == MediaKind::Tv {
            marker = marker_from_theintrodb(&snapshot);
            if let Some(value) = marker.clone() {
                markers.push(value);
            }
        }
        append_marker_chapters(&mut chapters, marker.as_ref());
        return ResolveOutput { chapters, markers };
    }

    let probed = probe_chapters(Path::new(&snapshot.row.path));
    if let Some(ref probed) = probed {
        chapters = annotate_chapters(probed);
        tracing::info!(
            media = %snapshot.media.title,
            path = %snapshot.row.path,
            count = chapters.len(),
            "【片头片尾】内嵌章节探测成功"
        );
    } else {
        tracing::warn!(
            media = %snapshot.media.title,
            path = %snapshot.row.path,
            "【片头片尾】内嵌章节探测失败（文件不可达 / 无章节 / 远程超时）"
        );
    }
    let has_intro = chapters
        .iter()
        .any(|chapter| chapter.marker_type == Some(MarkerType::IntroStart));
    if marker.is_none() && !has_intro && snapshot.media.kind == MediaKind::Tv {
        marker = marker_from_theintrodb(&snapshot);
        if let Some(value) = marker.clone() {
            markers.push(value);
        }
    }
    // 声纹比对不在此同步链路执行（避免阻塞请求）：由后台异步探测任务
    // （ProbeManager）采集单集指纹、同季 ≥2 集后自动比对识别片头。
    if marker.is_none() {
        marker = marker_from_chapters(&snapshot, &chapters);
        if let Some(value) = marker.clone() {
            markers.push(value);
        }
    }
    append_marker_chapters(&mut chapters, marker.as_ref());
    chapters.sort_by_key(|chapter| chapter.start_ms);
    tracing::info!(
        media = %snapshot.media.title,
        path = %snapshot.row.path,
        chapters = chapters.len(),
        markers = markers.len(),
        "【片头片尾】解析完成：章节 {} 条，片头片尾标记 {} 条",
        chapters.len(),
        markers.len()
    );
    ResolveOutput { chapters, markers }
}

fn marker_from_theintrodb(snapshot: &ResolveSnapshot) -> Option<StoredMediaMarker> {
    let client = snapshot.theintrodb.as_ref()?;
    let tmdb_id = snapshot.media.tmdb_id.as_deref()?;
    let season = snapshot.row.season.unwrap_or(1);
    let episode = snapshot.row.episode.unwrap_or(1);
    match client.get_episode_markers(tmdb_id, season, episode) {
        Ok(Some(result)) => Some(StoredMediaMarker {
            media_id: snapshot.media.id,
            season,
            episode,
            intro_start_ms: result.intro_start_ms,
            intro_end_ms: result.intro_end_ms,
            outro_start_ms: result.outro_start_ms,
            outro_end_ms: result.outro_end_ms,
            source: "theintrodb".into(),
            locked: false,
            updated_at: 0,
        }),
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(%error, title = %snapshot.media.title, season, episode, "查询 TheIntroDB 失败");
            None
        }
    }
}

fn marker_from_chapters(
    snapshot: &ResolveSnapshot,
    chapters: &[ChapterMarker],
) -> Option<StoredMediaMarker> {
    let intro = chapters
        .iter()
        .find(|chapter| chapter.marker_type == Some(MarkerType::IntroStart));
    let outro = chapters
        .iter()
        .find(|chapter| chapter.marker_type == Some(MarkerType::CreditsStart));
    (intro.is_some() || outro.is_some()).then(|| StoredMediaMarker {
        media_id: snapshot.media.id,
        season: snapshot.row.season.unwrap_or(1),
        episode: snapshot.row.episode.unwrap_or(1),
        intro_start_ms: intro.map(|chapter| chapter.start_ms),
        intro_end_ms: intro.map(|chapter| chapter.end_ms),
        outro_start_ms: outro.map(|chapter| chapter.start_ms),
        outro_end_ms: outro.map(|chapter| chapter.end_ms),
        source: "chapter".into(),
        locked: false,
        updated_at: 0,
    })
}

fn append_marker_chapters(chapters: &mut Vec<ChapterMarker>, marker: Option<&StoredMediaMarker>) {
    let Some(marker) = marker else { return };
    let intro = match (marker.intro_start_ms, marker.intro_end_ms) {
        (Some(s), Some(e)) if e > s => Some((s, e)),
        _ => None,
    };
    let outro = match (marker.outro_start_ms, marker.outro_end_ms) {
        (Some(s), Some(e)) if e > s => Some((s, e)),
        (Some(s), None) => Some((s, s + 60_000)),
        _ => None,
    };
    // 自动为无章节视频构造完整时间轴分段（[序幕 ->] 片头 -> 正片 [-> 片尾]）
    *chapters = marker::build_complete_timeline_chapters(chapters, intro, outro, None);
}
