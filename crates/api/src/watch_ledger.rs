use std::path::PathBuf;

use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, Release};

use crate::Store;

pub fn record_paths(
    store: &Store,
    paths: impl IntoIterator<Item = library::TransferredFile>,
) -> Result<Vec<PathBuf>, String> {
    let mut inserted = Vec::new();
    for file in paths {
        let dest = file.path;
        let path_str = dest.display().to_string();
        // 该路径已有台账行（Transfer 时 probe 过、带 filter_score）：扫描
        // 绝不能按文件名覆盖更可靠的既有信息。复用已有行，仅保留。
        if store
            .ledger_by_path(&path_str)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            if let Ok(true) = store.clear_ledger_missing(&path_str) {
                inserted.push(dest);
            }
            continue;
        }
        let name = dest
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        // B14: 优先使用 scan 阶段经过 NFO/父目录 fallback 识别的 release，
        // 只有未预识别时才按 basename 重新解析。
        let parsed = file
            .identified_release
            .unwrap_or_else(|| release::parse(name));
        let mut incoming = media_from_release(&parsed);
        if let Some(identity) = file.nfo_identity {
            incoming.kind = identity.kind;
            incoming.title = identity.title.unwrap_or(incoming.title);
            incoming.year = identity.year;
            incoming.original_title = identity.original_title;
            incoming.tmdb_id = identity.tmdb_id;
            incoming.douban_id = identity.douban_id;
            incoming.tvdb_id = identity.tvdb_id;
            incoming.bangumi_id = identity.bangumi_id;
            incoming.anilist_id = identity.anilist_id;
        }
        let media = store
            .resolve_scanned_media(incoming)
            .map_err(|e| e.to_string())?;
        let now_ms = crate::job_loop::unix_now() * 1000;
        store
            .insert_scanned_ledger(&LedgerRow {
                id: LedgerId::new(),
                media_id: media.id,
                path: path_str,
                season: parsed.season,
                episode: parsed.episode,
                resolution: parsed.resolution,
                codec: parsed.codec,
                hdr: parsed.hdr,
                quality_source: QualitySource::Release,
                confidence: parsed.confidence,
                filter_score: None,
            }, now_ms)
            .map_err(|e| e.to_string())?;
        inserted.push(dest);
    }
    Ok(inserted)
}

/// 「其他」库扫描：每个视频文件一个 Media（kind=video，标题 = 文件名主干，
/// 不做任何识别/刮削）。
pub fn record_video_paths(
    store: &Store,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<Vec<PathBuf>, String> {
    let mut inserted = Vec::new();
    for dest in paths {
        let path_str = dest.display().to_string();
        // 已有台账行：复用既有 Media ID（播放进度/收藏挂在它上面），
        // 绝不再建一个无别名的新 Media 顶替（P2：重扫换 Media ID）。
        if store
            .ledger_by_path(&path_str)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            if let Ok(true) = store.clear_ledger_missing(&path_str) {
                inserted.push(dest);
            }
            continue;
        }
        let stem = dest
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let media = store
            .ensure_media(Media {
                id: MediaId::new(),
                kind: MediaKind::Video,
                title: stem,
                year: None,
                original_title: None,
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .map_err(|e| e.to_string())?;
        let now_ms = crate::job_loop::unix_now() * 1000;
        store
            .insert_scanned_ledger(&LedgerRow {
                id: LedgerId::new(),
                media_id: media.id,
                path: dest.display().to_string(),
                season: None,
                episode: None,
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
            }, now_ms)
            .map_err(|e| e.to_string())?;
        inserted.push(dest);
    }
    Ok(inserted)
}

fn media_from_release(parsed: &Release) -> Media {
    Media {
        id: MediaId::new(),
        kind: if parsed.season.is_some() {
            MediaKind::Tv
        } else {
            MediaKind::Movie
        },
        title: parsed.title.clone(),
        year: parsed.year,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}
