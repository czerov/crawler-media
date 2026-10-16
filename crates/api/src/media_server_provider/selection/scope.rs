//! 请求作用域：一次 ownership + 可见库 + 作用域内的候选行。
//!
//! 媒体交集（`SeriesId` / 剧 parent）在 SQL 层收敛到索引化的 `ledger_for_media`；
//! 库与 folder 只作为附加归属约束，不再为每个 series 重读整库。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use domain::{LedgerRow, Media, MediaId, MediaKind, UserId};
use media_server::provider::query::folder_id_for_root;
use media_server::provider::ServerItemQuery;

pub(super) struct ScopedRead {
    pub(super) ownership: store::LibraryOwnership,
    pub(super) is_admin: bool,
    pub(super) user_id: UserId,
    pub(super) media: HashMap<MediaId, Media>,
    pub(super) rows: Vec<LedgerRow>,
    pub(super) units: Vec<crate::store::PlaybackUnitSnapshot>,
    /// 库作用域（`ParentId=<library id>`）：行还必须属于这个库。
    pub(super) requested_library: Option<String>,
    /// folder 作用域：行必须在这个根下（在折叠 series **之前**判定）。
    pub(super) folder_root: Option<PathBuf>,
    /// 读被收敛到单个 Media（`ParentId=<series>` 或 `SeriesId`）：一行一条，不折叠。
    pub(super) media_scoped: bool,
    /// parent 认不出来（含不可见 folder）：空集合，绝不整库。
    pub(super) unresolved: bool,
    /// parent 既不是库/剧/folder：要靠 item metadata 判 genre/studio。
    pub(super) unknown_parent: bool,
}

pub(super) fn read_scope(
    store: &crate::Store,
    user_id: UserId,
    query: &ServerItemQuery,
) -> Result<ScopedRead, String> {
    let ownership = store.library_ownership().map_err(|error| error.to_string())?;
    let is_admin = store.user_role(user_id).map_err(|error| error.to_string())? == "admin";
    let libraries = visible_libraries(store, &ownership, user_id, is_admin)?;
    let parent = query.parent_id.as_deref();
    let is_folder_id = parent.is_some_and(|value| value.starts_with("folder_"));
    let library = parent.filter(|id| libraries.iter().any(|library| library.id == *id));
    let folder = if is_folder_id {
        parent.and_then(|value| folder_root_in(value, &libraries))
    } else {
        None
    };
    // Series 判定与 `resolve_parent_scope` 一致：parent 本身是可见的 TV Media。
    let series_parent = match (parent, library, folder.as_ref()) {
        (Some(id), None, None) if !is_folder_id => series_media_id(store, user_id, id)?,
        _ => None,
    };
    // 媒体交集（`SeriesId` / 剧 parent）直接下推到 SQL；库与 folder 只是附加归属约束。
    // parent 与 SeriesId 同时给出时，非库/folder 的 parent 优先（旧 `parent.or(series)`）。
    let series_param = query.series_id.as_deref();
    let media_target: Option<String> = if library.is_some() || is_folder_id {
        series_param.map(str::to_owned)
    } else if let Some(series) = series_param {
        Some(parent.unwrap_or(series).to_string())
    } else {
        series_parent.map(|_| parent.unwrap_or_default().to_string())
    };
    let media_scope = media_target.as_deref().and_then(|id| id.parse::<MediaId>().ok());
    if media_target.is_some() && media_scope.is_none() {
        // 作用域指向的 id 解析不出 Media（例如随便给的 SeriesId）：空集合，绝不整库。
        return Ok(unresolved_scope(ownership, is_admin, user_id));
    }
    let (rows, requested_library, folder_root) = if let Some(media_id) = media_scope {
        (
            store
                .ledger_for_media(media_id)
                .map_err(|error| error.to_string())?,
            library.map(str::to_owned),
            folder,
        )
    } else if let Some(library_id) = library {
        (
            store
                .ledger_candidates_for_library(&ownership, library_id)
                .map_err(|error| error.to_string())?,
            Some(library_id.to_string()),
            None,
        )
    } else if is_folder_id {
        match folder {
            Some(root) => {
                // 只读该根所属库的候选，再由前缀判定归属；folder 语义本身不限制库。
                let owner = libraries
                    .iter()
                    .find(|library| library.root_paths.iter().any(|path| path == &root));
                let rows = match owner {
                    Some(library) => store
                        .ledger_candidates_for_library(&ownership, &library.id)
                        .map_err(|error| error.to_string())?,
                    None => Vec::new(),
                };
                (rows, None, Some(root))
            }
            // 认不出的 folder：空集合，绝不整库。
            None => return Ok(unresolved_scope(ownership, is_admin, user_id)),
        }
    } else {
        (
            store
                .jellyfin_ledger_candidates(&ownership, None)
                .map_err(|error| error.to_string())?,
            None,
            None,
        )
    };
    let ids: Vec<MediaId> = rows.iter().map(|row| row.media_id).collect();
    let media = store.media_batch(&ids).map_err(|error| error.to_string())?;
    let units = store
        .playback_units_for_media(user_id, &ids)
        .map_err(|error| error.to_string())?;
    let unknown_parent =
        parent.is_some() && library.is_none() && !is_folder_id && media_target.is_none();
    Ok(ScopedRead {
        ownership,
        is_admin,
        user_id,
        media,
        rows,
        units,
        requested_library,
        folder_root,
        media_scoped: media_scope.is_some(),
        unresolved: false,
        unknown_parent,
    })
}

pub(super) fn unresolved_scope(
    ownership: store::LibraryOwnership,
    is_admin: bool,
    user_id: UserId,
) -> ScopedRead {
    ScopedRead {
        ownership,
        is_admin,
        user_id,
        media: HashMap::new(),
        rows: Vec::new(),
        units: Vec::new(),
        requested_library: None,
        folder_root: None,
        media_scoped: false,
        unresolved: true,
        unknown_parent: false,
    }
}

/// 用户可见的库（口径与 `list_visible_libraries` 一致；这里只需要 id/kind/根路径）。
fn visible_libraries(
    store: &crate::Store,
    ownership: &store::LibraryOwnership,
    user_id: UserId,
    is_admin: bool,
) -> Result<Vec<crate::store::Library>, String> {
    let libraries = store.list_libraries().map_err(|error| error.to_string())?;
    Ok(libraries
        .into_iter()
        .filter(|library| {
            ownership.accepts_work(&library.id)
                && if is_admin {
                    library.admin_visible
                } else {
                    library.access_mode == "everyone" || library.member_ids.contains(&user_id)
                }
        })
        .collect())
}

/// `folder_<uuid5(root)>` 在**可见**库根里的匹配（folder id 方案来自协议层）。
fn folder_root_in(parent: &str, libraries: &[crate::store::Library]) -> Option<PathBuf> {
    libraries.iter().find_map(|library| {
        library
            .root_paths
            .iter()
            .find_map(|root| (folder_id_for_root(root) == parent).then(|| root.clone()))
    })
}

/// parent 是可见的 TV Media 本身（`resolve_single_item` 的 `is_series` 口径）。
fn series_media_id(
    store: &crate::Store,
    user_id: UserId,
    id: &str,
) -> Result<Option<MediaId>, String> {
    let Some(row) = crate::http::media_visibility::resolve_visible_row(store, id, Some(user_id))
    else {
        return Ok(None);
    };
    let Some(media) = store.get_media(row.media_id).map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    let compact = id.replace('-', "");
    let is_series = media.kind == MediaKind::Tv
        && (compact == media.id.to_string().replace('-', "") || id == media.id.to_string());
    Ok(is_series.then_some(media.id))
}

pub(super) fn in_scope_folder(scope: &ScopedRead, row: &LedgerRow) -> bool {
    scope
        .folder_root
        .as_ref()
        .is_none_or(|root| Path::new(&row.path).starts_with(root))
}

pub(super) fn row_visible(scope: &ScopedRead, row: &LedgerRow) -> bool {
    let Some(media) = scope.media.get(&row.media_id) else {
        return false;
    };
    let Some(owner) = scope.ownership.owner(Path::new(&row.path), media.kind) else {
        return false;
    };
    scope.ownership.accepts_work(&owner.id)
        && scope
            .requested_library
            .as_ref()
            .is_none_or(|library_id| &owner.id == library_id)
        && if scope.is_admin {
            owner.admin_visible
        } else {
            owner.access_mode == "everyone" || owner.member_ids.contains(&scope.user_id)
        }
}
