//! Auto-resolve media metadata against TMDB/Catalog during library scans.
//! Inspired by MovieClaw & MoviePilot two-stage identification.

use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;
use domain::{Media, MediaKind};

/// Automatically look up TMDB metadata for a media item if it lacks external IDs.
/// If an exact or high-confidence match is found:
/// 1. Updates `media` in the database with `tmdb_id`, `year`, `original_title`
/// 2. Downloads and writes the official poster beside the preferred file
pub fn auto_resolve_media(
    state: &ApiState,
    media: &Media,
    sample_path: &std::path::Path,
) -> Option<Media> {
    resolve_media(state, media, sample_path, true)
}

/// Scans publish sidecars only after rebinding their newly inserted ledger rows.
pub(crate) fn resolve_scanned_media(
    state: &ApiState,
    media: &Media,
    sample_path: &std::path::Path,
) -> Option<Media> {
    resolve_media(state, media, sample_path, false)
}

fn resolve_media(
    state: &ApiState,
    media: &Media,
    sample_path: &std::path::Path,
    publish_sidecars: bool,
) -> Option<Media> {
    let lease = state.acquire_path_work(sample_path, media.kind).ok()?;
    if has_local_scan_metadata(state, media, sample_path)
        || media.tmdb_id.is_some()
        || media.douban_id.is_some()
    {
        return Some(media.clone());
    }

    let title = media.title.trim();
    if title.is_empty() {
        return None;
    }

    tracing::info!(
        title = %title,
        kind = ?media.kind,
        "扫描时自动解析媒体元数据（TMDB/目录）"
    );

    let queries = search_queries(title, sample_path);
    let mut best_hit = None;
    for query in &queries {
        let hits = match media.kind {
            MediaKind::Movie => state.catalog.search_movie(query).ok(),
            MediaKind::Tv => state.catalog.search_tv_year(query, media.year).ok(),
            MediaKind::Video => None,
        };
        let Some(hits) = hits else { continue };
        if let Some(hit) = find_best_catalog_match(query, media.year, &hits) {
            best_hit = Some(hit.clone());
            break;
        }
    }
    if lease
        .as_ref()
        .is_some_and(|lease| lease.control().is_cancelled())
    {
        return None;
    }
    let Some(best_hit) = best_hit else {
        tracing::debug!(title = %title, year = ?media.year, "扫描时未找到可靠的 TMDB 匹配");
        if media.kind == MediaKind::Tv {
            attach_episode_frames(state, media, sample_path);
        }
        return None;
    };

    let tmdb_id = best_hit.media.tmdb_id.clone()?;
    tracing::info!(
        title = %title,
        matched_title = %best_hit.media.title,
        tmdb_id = %tmdb_id,
        year = ?best_hit.media.year,
        "成功匹配 TMDB 媒体"
    );

    let mut updated = media.clone();
    if updated.tmdb_id.is_none() {
        updated.tmdb_id = best_hit.media.tmdb_id.clone();
    }
    if updated.douban_id.is_none() {
        updated.douban_id = best_hit.media.douban_id.clone();
    }
    if updated.year.is_none() {
        updated.year = best_hit.media.year;
    }
    if updated.original_title.is_none() {
        updated.original_title = best_hit.media.original_title.clone();
    }

    // External IDs are aliases: an online match may already have a canonical
    // Media with Playback history. The scan caller rebinds only its new rows.
    let updated = {
        let store = state.store.lock();
        if lease
            .as_ref()
            .is_some_and(|lease| lease.control().is_cancelled())
        {
            return None;
        }
        match store.resolve_scanned_media(updated) {
            Ok(canonical) => canonical,
            Err(error) => {
                tracing::error!(%error, media_id = %media.id, "无法汇合扫描媒体身份");
                return None;
            }
        }
    };

    if !publish_sidecars {
        return Some(updated);
    }

    // Attach official poster & backdrop
    let _ = crate::poster_fetch::attach_poster(state, &updated, sample_path);
    let _ = crate::poster_fetch::attach_backdrop(state, &updated, sample_path);

    // Write NFO metadata beside video files and series root
    let nfo_enabled = state
        .store
        .lock()
        .get_scrape_config()
        .ok()
        .map(|config| config.effective.mirror_nfo)
        .unwrap_or(true);
    if nfo_enabled {
        write_auto_resolved_nfo(state, &updated, sample_path);
    }

    Some(updated)
}

fn write_auto_resolved_nfo(state: &ApiState, media: &Media, sample_path: &std::path::Path) {
    let lease = match state.acquire_path_work(sample_path, media.kind) {
        Ok(lease) => lease,
        Err(error) => {
            tracing::warn!(path = %sample_path.display(), %error, "自动 NFO 写入被拒绝");
            return;
        }
    };
    let Some(tmdb_id) = media.tmdb_id.as_deref() else {
        return;
    };
    let Ok(Some(metadata)) =
        crate::scrape_metadata::fetch_tmdb_metadata(state, media.kind, tmdb_id)
    else {
        return;
    };
    let nfo = crate::scrape_metadata::nfo_from_tmdb(media, &metadata);
    if media.kind == MediaKind::Tv {
        let (rows, sample_row) = {
            let store = state.store.lock();
            let all = store.ledger_for_media(media.id).unwrap_or_default();
            let sample_path_str = sample_path.display().to_string();
            let sample = all.iter().find(|r| r.path == sample_path_str).cloned();
            (all, sample)
        };
        let sample_row = sample_row.unwrap_or_else(|| {
            let (season, episode) = sample_path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|name| {
                    let parsed = release::parse(name);
                    (parsed.season, parsed.episode)
                })
                .unwrap_or((None, None));
            domain::LedgerRow {
                id: domain::LedgerId::new(),
                media_id: media.id,
                path: sample_path.display().to_string(),
                season,
                episode,
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: domain::QualitySource::Release,
                confidence: domain::Confidence::High,
                filter_score: None,
            }
        });
        let show_root = crate::scrape_metadata::show_root(sample_path, &sample_row);
        let rows_to_write = if rows.is_empty() {
            vec![sample_row]
        } else {
            rows
        };
        crate::scrape_metadata::write_series_nfos(
            state,
            media,
            tmdb_id,
            &show_root,
            &rows_to_write,
            &nfo,
            &crate::scrape_metadata::preferred_language(state),
        );
    } else if let Some(stem) = sample_path.file_stem().and_then(|value| value.to_str()) {
        let target = sample_path.with_file_name(format!("{stem}.nfo"));
        let write = || {
            crate::scrape_metadata::write_nfo(&target, media, &nfo);
            Ok(())
        };
        let result = match &lease {
            Some(lease) => lease.publish(write),
            None => write(),
        };
        if let Err(error) = result {
            tracing::warn!(path = %target.display(), %error, "自动电影 NFO 写入被取消");
        }
    }
}

/// 目录对不上时，给同一部剧还没有剧照的分集各截一帧。
/// STRM 走里面的远程地址，和流探测同一套输入，不把 `.strm` 文本当视频。
fn attach_episode_frames(state: &ApiState, media: &Media, sample_path: &std::path::Path) {
    let Some(dir) = sample_path.parent() else {
        return;
    };
    let paths = {
        let store = state.store.lock();
        store
            .list_ledger()
            .unwrap_or_default()
            .into_iter()
            .filter(|row| row.media_id == media.id)
            .map(|row| std::path::PathBuf::from(row.path))
            .filter(|path| path.parent() == Some(dir))
            .collect::<Vec<_>>()
    };
    let targets = if paths.is_empty() {
        vec![sample_path.to_path_buf()]
    } else {
        paths
    };
    for path in targets {
        let lease = match state.acquire_path_work(&path, media.kind) {
            Ok(lease) => lease,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "分集抓帧被拒绝");
                continue;
            }
        };
        let control = lease
            .as_ref()
            .map(|lease| lease.control().clone())
            .unwrap_or_default();
        let still = crate::episode_still::path(&path);
        if still.is_file() {
            continue;
        }
        if let Err(error) = library::extract_frame_controlled(&path, 60_000, &still, &control) {
            tracing::warn!(path = %path.display(), %error, "匹配失败后分集抓帧失败");
        }
    }
}

fn find_best_catalog_match<'a>(
    title: &str,
    year: Option<u16>,
    hits: &'a [media::CatalogHit],
) -> Option<&'a media::CatalogHit> {
    if hits.is_empty() {
        return None;
    }

    let norm_target = normalize_title(title);
    let eligible: Vec<_> = hits
        .iter()
        .filter(|hit| year.is_none_or(|target| hit.media.year.is_none_or(|found| found == target)))
        .collect();

    // 1. Exact title match + year match
    if let Some(target_year) = year {
        for hit in &eligible {
            if hit.media.year == Some(target_year)
                && normalize_title(&hit.media.title) == norm_target
            {
                return Some(hit);
            }
        }
    }

    // 2. Exact title match (regardless of year)
    for hit in &eligible {
        if normalize_title(&hit.media.title) == norm_target {
            return Some(hit);
        }
    }

    // 3. If only 1 hit returned and title contains target or target contains title
    if eligible.len() == 1 {
        let hit_title = normalize_title(&eligible[0].media.title);
        if hit_title.contains(&norm_target) || norm_target.contains(&hit_title) {
            return Some(eligible[0]);
        }
    }

    None
}

/// 文件名标题优先；上层目录里和它不同的标题（常见是中文别名）作为后续搜索词。
fn search_queries(title: &str, sample_path: &std::path::Path) -> Vec<String> {
    let mut queries = vec![title.to_string()];
    let mut current = sample_path.parent();
    let mut depth = 0;
    while let Some(dir) = current {
        if depth >= 4 {
            break;
        }
        depth += 1;
        if let Some(name) = dir.file_name().and_then(|name| name.to_str()) {
            let parsed = release::parse(name);
            let candidate = parsed.title.trim();
            if parsed.confidence != domain::Confidence::Low
                && !candidate.is_empty()
                && normalize_title(candidate) != normalize_title(title)
                && !queries
                    .iter()
                    .any(|query| normalize_title(query) == normalize_title(candidate))
            {
                queries.push(candidate.to_string());
            }
        }
        current = dir.parent();
    }
    queries
}

fn normalize_title(t: &str) -> String {
    t.chars()
        .filter(|c| {
            !c.is_whitespace() && *c != '.' && *c != '-' && *c != '_' && *c != ':' && *c != '：'
        })
        .collect::<String>()
        .to_lowercase()
}

/// Complete local identity and a display title are enough; missing plot/cast
/// never implicitly turns a normal scan into an explicit metadata refresh.
pub(crate) fn has_local_scan_metadata(
    state: &ApiState,
    media: &domain::Media,
    path: &std::path::Path,
) -> bool {
    let root = {
        let store = state.store.lock();
        store
            .library_for_path_strict(path, media.kind)
            .ok()
            .flatten()
            .and_then(|library| {
                library
                    .root_paths
                    .into_iter()
                    .find(|root| path.starts_with(root))
            })
    };
    let Some(root) = root else { return false };
    match library::read_watch_identity(path, &root, media.kind) {
        Ok(Some(identity)) => {
            identity.title.is_some()
                && [
                    identity.tmdb_id,
                    identity.douban_id,
                    identity.tvdb_id,
                    identity.bangumi_id,
                    identity.anilist_id,
                ]
                .iter()
                .any(Option::is_some)
        }
        Ok(None) => false,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "无法读取本地 NFO，回退扫描识别");
            false
        }
    }
}

pub(crate) fn rebind_scan_rows(
    state: &ApiState,
    previous: domain::MediaId,
    canonical: domain::MediaId,
    inserted: &[std::path::PathBuf],
) -> Result<(), String> {
    if previous == canonical {
        return Ok(());
    }
    let store = state.store.lock();
    for path in inserted {
        let Some(row) = store
            .ledger_by_path(&path.display().to_string())
            .map_err(|e| e.to_string())?
            .filter(|row| row.media_id == previous)
        else {
            continue;
        };
        let kind = store
            .get_media(canonical)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "canonical Media missing".to_string())?
            .kind;
        let owner = store
            .library_for_path_strict(path, kind)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "scanned path no longer owned".to_string())?;
        if !store
            .library_accepts_work(&owner.id)
            .map_err(|e| e.to_string())?
        {
            return Err("Library 正在删除".into());
        }
        store
            .rebind_scanned_ledger(row.id, canonical)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
