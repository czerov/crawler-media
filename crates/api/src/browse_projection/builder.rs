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
use store::{BrowseFileFacts, BrowseMediaFacts, CatalogCacheRow, ProjectionBuild, ReadRevisions};

use crate::browse_file_read::BrowseFileRead;
use crate::http::library::selection::metadata::{self, CacheIndex};
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
    let media_map = load_media(state, &owned.media_ids);
    let catalog_rows = load_catalog_rows(state, &media_map);
    let cache = CacheIndex::build(&catalog_rows);
    let files = state.browse_files.as_ref();
    let mut rows_by_media: HashMap<MediaId, Vec<LedgerRow>> = HashMap::new();
    let mut file_facts = Vec::with_capacity(owned.rows.len());
    for row in &owned.rows {
        rows_by_media
            .entry(row.media_id)
            .or_default()
            .push(row.clone());
        file_facts.push(file_facts_for(files, row, owned.missing.contains(&row.path)));
    }
    let media_facts = media_facts_for(&owned.media_ids, &media_map, &rows_by_media, &cache, files);
    Ok(ProjectionBuild {
        library_id: library_id.to_string(),
        generation: 1,
        scope_signature: scope_signature(&owned.library),
        revisions: owned.revisions,
        files: file_facts,
        media: media_facts,
    })
}

/// 锁内抓 owned 事实，锁外用与旧浏览路径**同一**归属口径过滤 ledger 行。
///
/// 必须用 [`store::LibraryOwnership::owner`]（未命中任何根时回退到该 kind 的默认
/// Library），而不是 scan 用的 strict scope：旧 `load_browse_snapshot` 就是这个口径，
/// 换成 strict 会把「不在任何根下」的 ledger 行整批丢掉，浏览输出随之变化。
fn collect_owned(state: &ApiState, library_id: &str) -> Result<Owned, ProjectionError> {
    let (library, snapshot, candidate_rows, missing, revisions) = {
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
        let rows = store
            .list_ledger()
            .map_err(|e| ProjectionError::Failed(e.to_string()))?;
        let missing = store
            .missing_at_by_path()
            .map_err(|e| ProjectionError::Failed(e.to_string()))?
            .into_keys()
            .collect();
        let revs = store
            .read_revisions()
            .map_err(|e| ProjectionError::Failed(e.to_string()))?;
        (library, snapshot, rows, missing, revs)
    };

    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    let mut media_ids = Vec::new();
    for row in candidate_rows {
        let owner = snapshot.owner(Path::new(&row.path), library.kind);
        if owner.is_some_and(|owner| owner.id == library_id) {
            if seen.insert(row.media_id) {
                media_ids.push(row.media_id);
            }
            rows.push(row);
        }
    }
    Ok(Owned {
        library,
        rows,
        media_ids,
        missing,
        revisions,
    })
}

/// Media 行按 500 一批读取，避免超过 SQLite 变量上限。
fn load_media(state: &ApiState, ids: &[MediaId]) -> HashMap<MediaId, Media> {
    let store = state.store.lock();
    let mut map = HashMap::new();
    for chunk in ids.chunks(500) {
        match store.media_batch(chunk) {
            Ok(batch) => map.extend(batch),
            Err(error) => tracing::warn!(%error, "投影构建读取 Media 批次失败"),
        }
    }
    map
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

fn file_facts_for(files: &dyn BrowseFileRead, row: &LedgerRow, missing: bool) -> BrowseFileFacts {
    let path = Path::new(&row.path);
    let (size, has_stat, created_ns, modified_ns) = observe(files, path);
    let facts = FileFacts {
        row: row.clone(),
        size,
        has_stat,
        created_ns,
        modified_ns,
        missing,
        has_still: crate::episode_still::existing_with(path, files).is_some(),
    };
    BrowseFileFacts {
        ledger_id: row.id,
        media_id: row.media_id,
        facts: facts.to_json(),
    }
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

fn media_facts_for(
    media_ids: &[MediaId],
    media_map: &HashMap<MediaId, Media>,
    rows_by_media: &HashMap<MediaId, Vec<LedgerRow>>,
    cache: &CacheIndex<'_>,
    files: &dyn BrowseFileRead,
) -> Vec<BrowseMediaFacts> {
    let empty: Vec<LedgerRow> = Vec::new();
    let mut out = Vec::with_capacity(media_ids.len());
    for mid in media_ids {
        let Some(media) = media_map.get(mid) else {
            continue;
        };
        let rows = rows_by_media.get(mid).unwrap_or(&empty);
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
        out.push(BrowseMediaFacts {
            media_id: *mid,
            title: media.title.clone(),
            year: media.year,
            facts: facts.to_json(),
        });
    }
    out
}

/// Scope 身份指纹：kind + 排序后的根路径。读路径据此拒绝跨 scope 的旧代。
pub(crate) fn scope_signature(library: &Library) -> String {
    let mut roots: Vec<String> = library
        .root_paths
        .iter()
        .map(|root| root.display().to_string())
        .collect();
    roots.sort_unstable();
    format!("{}|{}", library.kind.as_str(), roots.join("\u{1f}"))
}
