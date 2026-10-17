//! Off-lock browse projection builder.
//!
//! 构建器在锁内只取 owned 的 DB 事实（ownership/ledger/missing/Media/revision 与
//! 目标 Media 的 catalog 键），NFO 读取、文件 stat、海报/剧照探测、catalog 解析
//! 全部在锁外通过 [`crate::browse_file_read::BrowseFileRead`] 完成。读路径因此
//! 不再需要任何文件 IO。

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::UNIX_EPOCH;

use domain::{LedgerRow, Media, MediaId};
use store::{
    BrowseFileFacts, BrowseMediaCandidate, BrowseMediaFacts, CatalogCacheRow, ProjectionBuild,
    ReadRevisions,
};

use crate::browse_file_read::BrowseFileRead;
use crate::http::library::selection::metadata::{self, CacheIndex};
use crate::http::library::selection::{LightFacts, is_provisional};
use crate::http::library::selection::projected::total_size;
use crate::http::library::{backdrop_path_with, poster_path_with, preferred_row};
use crate::management::ApiState;
use crate::store::Library;
use super::facts::{FileFacts, MediaFacts};
use super::ProjectionError;

struct Owned {
    library: Library,
    rows: Vec<LedgerRow>,
    media_ids: Vec<MediaId>,
    missing: HashSet<String>,
    revisions: ReadRevisions,
}

pub fn build(state: &ApiState, library_id: &str) -> Result<ProjectionBuild, ProjectionError> {
    let owned = collect_owned(state, library_id)?;
    let media_map = load_media(state, &owned.media_ids)?;
    let catalog_rows = load_catalog_rows(state, &media_map);
    let cache = CacheIndex::build(&catalog_rows);
    let files = state.browse_files.as_ref();
    let mut rows_by_media: HashMap<MediaId, Vec<LedgerRow>> = HashMap::new();
    let mut row_facts_by_media: HashMap<MediaId, Vec<FileFacts>> = HashMap::new();
    let mut file_facts = Vec::with_capacity(owned.rows.len());
    for row in &owned.rows {
        rows_by_media
            .entry(row.media_id)
            .or_default()
            .push(row.clone());
        if let Some(media) = media_map.get(&row.media_id) {
            let facts = file_facts_for(files, row, media, owned.missing.contains(&row.path));
            file_facts.push(BrowseFileFacts {
                ledger_id: row.id,
                media_id: row.media_id,
                path: row.path.clone(),
                facts: facts.to_json(),
            });
            row_facts_by_media
                .entry(row.media_id)
                .or_default()
                .push(facts);
        }
    }
    let (media_facts, candidates) = media_facts_for(
        &owned.media_ids,
        &media_map,
        &rows_by_media,
        &row_facts_by_media,
        &cache,
        files,
    );
    let (expected_dirty_token, next_generation) = {
        let store = state.store.lock();
        let head = store
            .browse_projection_head(library_id)
            .map_err(|e| ProjectionError::Failed(e.to_string()))?;
        match head {
            Some(h) => (Some(h.dirty_token), h.generation + 1),
            None => (None, 1),
        }
    };
    Ok(ProjectionBuild {
        library_id: library_id.to_string(),
        generation: next_generation,
        // scope 指纹与发布/新鲜度检查共用 store 的实现，避免两处口径分叉。
        scope_signature: store::library_scope_signature(&owned.library),
        revisions: owned.revisions,
        expected_dirty_token,
        files: file_facts,
        media: media_facts,
        candidates,
    })
}

/// 锁内抓 owned 事实，锁外用与旧浏览路径**同一**归属口径过滤 ledger 行。
///
/// 必须用 [`store::LibraryOwnership::owner`]（未命中任何根时回退到该 kind 的默认
/// Library），而不是 scan 用的 strict scope：旧 `load_browse_snapshot` 就是这个口径，
/// 换成 strict 会把「不在任何根下」的 ledger 行整批丢掉，浏览输出随之变化。
///
/// 候选行由 [`store::Store::ledger_candidates_for_library`] 定向读取：**非默认**
/// Library 的无关 ledger 行不进入内存，缺失标记也只查本库路径，因此无关台账增长
/// 不再增加非默认库构建的 DB 行数、锁持有时间与内存。
///
/// 默认 Library 是例外：它的回退分支是「不属于任何同 kind 根」的 `NOT (...)` 补集，
/// SQLite 无法 seek，成本仍随整个 ledger 增长（P3-D 已记录的限制，见
/// [`store::Store::ledger_candidates_for_library`] 文档与
/// `browse_reads::tests::default_library_candidate_read_is_not_bounded`）。
fn collect_owned(state: &ApiState, library_id: &str) -> Result<Owned, ProjectionError> {
    let store = state.store.lock();
    let snapshot = store
        .library_ownership()
        .map_err(|e| ProjectionError::Failed(e.to_string()))?;
    let library = snapshot
        .library(library_id)
        .cloned()
        .ok_or(ProjectionError::Cancelled)?;
    if !snapshot.accepts_work(library_id) {
        return Err(ProjectionError::Cancelled);
    }
    let candidates = store
        .ledger_candidates_for_library(&snapshot, library_id)
        .map_err(|e| ProjectionError::Failed(e.to_string()))?;

    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    let mut media_ids = Vec::new();
    for row in candidates {
        let owner = snapshot.owner(Path::new(&row.path), library.kind);
        if owner.is_some_and(|owner| owner.id == library_id) {
            if seen.insert(row.media_id) {
                media_ids.push(row.media_id);
            }
            rows.push(row);
        }
    }
    let paths: Vec<String> = rows.iter().map(|row| row.path.clone()).collect();
    let missing = store
        .missing_at_for_paths(&paths)
        .map_err(|e| ProjectionError::Failed(e.to_string()))?;
    let revisions = store
        .read_revisions()
        .map_err(|e| ProjectionError::Failed(e.to_string()))?;
    drop(store);

    Ok(Owned {
        library,
        rows,
        media_ids,
        missing,
        revisions,
    })
}

/// Media 行按 500 一批读取，避免超过 SQLite 变量上限。
fn load_media(state: &ApiState, ids: &[MediaId]) -> Result<HashMap<MediaId, Media>, ProjectionError> {
    let store = state.store.lock();
    let mut map = HashMap::new();
    for chunk in ids.chunks(500) {
        let batch = store
            .media_batch(chunk)
            .map_err(|error| ProjectionError::Failed(error.to_string()))?;
        map.extend(batch);
    }
    Ok(map)
}

/// 只查目标 Media 的详情键前缀：不再全量读取 catalog body。
fn load_catalog_rows(state: &ApiState, media_map: &HashMap<MediaId, Media>) -> Vec<CatalogCacheRow> {
    let mut prefixes: Vec<String> = Vec::new();
    for media in media_map.values() {
        let Some(tmdb_id) = &media.tmdb_id else { continue };
        let prefix = format!("/{}/{tmdb_id}", media.kind.as_str());
        if !prefixes.contains(&prefix) {
            prefixes.push(prefix);
        }
    }
    if prefixes.is_empty() {
        return Vec::new();
    }
    let store = state.store.lock();
    store
        .catalog_details_for_prefixes("tmdb", &prefixes)
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "投影构建读取 catalog 详情失败");
            Vec::new()
        })
}

fn file_facts_for(
    files: &dyn BrowseFileRead,
    row: &LedgerRow,
    media: &Media,
    missing: bool,
) -> FileFacts {
    let path = Path::new(&row.path);
    let (size, has_stat, created_ns, modified_ns) = observe(files, path);
    let mut sidecars = serde_json::Map::new();
    for candidate in crate::scrape_metadata::nfo_candidates(path, row, media.kind) {
        sidecars.insert(candidate.display().to_string(), observe_version(files, &candidate));
    }
    for candidate in artwork_candidates(path) {
        sidecars.insert(candidate.display().to_string(), observe_version(files, &candidate));
    }
    FileFacts {
        row: row.clone(),
        size,
        has_stat,
        created_ns,
        modified_ns,
        missing,
        has_still: crate::episode_still::existing_with(path, files).is_some(),
        sidecars,
    }
}

fn observe_version(files: &dyn BrowseFileRead, path: &Path) -> serde_json::Value {
    match files.metadata(path) {
        Ok(meta) if meta.is_file() => serde_json::json!({
            "size": meta.len(),
            "modified_ns": meta.modified().ok().and_then(to_ns),
        }),
        _ => serde_json::Value::Null,
    }
}

fn artwork_candidates(video: &Path) -> Vec<std::path::PathBuf> {
    let Some(parent) = video.parent() else {
        return Vec::new();
    };
    let stem = video.file_stem().and_then(|value| value.to_str()).unwrap_or_default();
    let mut candidates = vec![parent.join("poster.jpg"), parent.join("fanart.jpg")];
    if let Some(up) = parent.parent() {
        candidates.push(up.join("poster.jpg"));
        candidates.push(up.join("fanart.jpg"));
    }
    candidates.extend([
        crate::episode_still::path(video),
        parent.join(format!("{stem}-still.jpg")),
        parent.join(format!("{stem}.jpg")),
        parent.join("still.jpg"),
    ]);
    candidates
}

/// 与 `file_stats` 同一口径：只有普通文件计入 size/added_at，缺失或错误留空。
fn observe(
    files: &dyn BrowseFileRead,
    path: &Path,
) -> (u64, bool, Option<i64>, Option<i64>) {
    match files.metadata(path) {
        Ok(meta) if meta.is_file() => (
            meta.len(),
            true,
            meta.created().ok().and_then(to_ns),
            meta.modified().ok().and_then(to_ns),
        ),
        Ok(_) => {
            tracing::warn!(path = %path.display(), "Library 文件路径不是普通文件");
            (0, false, None, None)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (0, false, None, None),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "投影构建读取文件元数据失败");
            (0, false, None, None)
        }
    }
}

fn to_ns(time: std::time::SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_nanos() as i64)
}

/// 每个 Media 的投影事实与轻候选：同一遍算完，保证两者同源。
fn media_facts_for(
    media_ids: &[MediaId],
    media_map: &HashMap<MediaId, Media>,
    rows_by_media: &HashMap<MediaId, Vec<LedgerRow>>,
    row_facts_by_media: &HashMap<MediaId, Vec<FileFacts>>,
    cache: &CacheIndex<'_>,
    files: &dyn BrowseFileRead,
) -> (Vec<BrowseMediaFacts>, Vec<BrowseMediaCandidate>) {
    let empty_rows: Vec<LedgerRow> = Vec::new();
    let empty_facts: Vec<FileFacts> = Vec::new();
    let mut out = Vec::with_capacity(media_ids.len());
    let mut candidates = Vec::with_capacity(media_ids.len());
    for mid in media_ids {
        let Some(media) = media_map.get(mid) else {
            continue;
        };
        let rows = rows_by_media.get(mid).unwrap_or(&empty_rows);
        let row_facts = row_facts_by_media.get(mid).unwrap_or(&empty_facts);
        let (has_poster, has_backdrop) = match preferred_row(rows) {
            Some(row) => (
                poster_path_with(row, files).is_some(),
                backdrop_path_with(row, files).is_some(),
            ),
            None => (false, false),
        };
        let facts = MediaFacts {
            media: media.clone(),
            metadata: metadata::load(media, rows, cache, files),
            has_poster,
            has_backdrop,
        };
        candidates.push(candidate_for(media, &facts, rows, row_facts));
        out.push(BrowseMediaFacts {
            media_id: *mid,
            title: media.title.clone(),
            year: media.year,
            facts: facts.to_json(),
        });
    }
    (out, candidates)
}

/// 轻候选：过滤/排序字段的物化（无 LedgerRow、无整份 facts JSON）。
///
/// 轻事实（resolutions/hdr/release_date）与全量路径**同一个** [`LightFacts::from_rows`]，
/// 因此选页前的过滤排序与全量路径逐字段一致。
fn candidate_for(
    media: &Media,
    facts: &MediaFacts,
    rows: &[LedgerRow],
    row_facts: &[FileFacts],
) -> BrowseMediaCandidate {
    let light = LightFacts::from_rows(media, &facts.metadata, rows);
    let mut episode_keys: Vec<(u32, u32)> = row_facts
        .iter()
        .filter_map(|fact| fact.row.season.zip(fact.row.episode))
        .collect();
    episode_keys.sort_unstable();
    episode_keys.dedup();
    BrowseMediaCandidate {
        media_id: media.id,
        kind: media.kind,
        title: media.title.clone(),
        tmdb_id: media.tmdb_id.clone(),
        year: media.year,
        added_at_ns: row_facts.iter().filter_map(FileFacts::added_at_ns).min(),
        release_date: light.release_date,
        rating: facts.metadata.rating,
        runtime: facts.metadata.runtime,
        size_bytes: total_size(row_facts),
        provisional: is_provisional(media),
        scraped: facts.metadata.scraped,
        missing: row_facts.iter().any(|fact| fact.missing),
        hdr: light.hdr,
        resolutions: light.resolutions,
        genres: facts.metadata.genres.clone(),
        countries: facts.metadata.countries.clone(),
        language: facts.metadata.language.clone(),
        has_poster: facts.has_poster,
        has_backdrop: facts.has_backdrop,
        has_still: row_facts.iter().any(|fact| fact.has_still),
        episode_keys,
    }
}
