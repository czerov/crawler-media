//! 请求作用域：一次 ownership + 可见库 + 作用域内的候选行。
//!
//! 媒体交集（`SeriesId` / 剧 parent）在 SQL 层收敛到索引化的 `ledger_for_media`；
//! 库与 folder 只作为附加归属约束，不再为每个 series 重读整库。

use std::collections::{HashMap, HashSet};
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
    pub(super) requested_library: Option<String>,
    pub(super) folder_root: Option<PathBuf>,
    pub(super) media_scoped: bool,
    pub(super) unresolved: bool,
    pub(super) unknown_parent: bool,
    pub(super) rows_read: usize,
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
    if is_folder_id && folder.is_none() {
        return Ok(unresolved_scope(ownership, is_admin, user_id));
    }
    let (series_parent, classified_rows) = match (parent, library, folder.as_ref()) {
        (Some(id), None, None) if !is_folder_id => {
            series_media_id(store, user_id, id, &ownership, is_admin)?
        }
        _ => (None, None),
    };
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
        return Ok(unresolved_scope(ownership, is_admin, user_id));
    }
    let (rows, requested_library, folder_root) = if let Some(rows) = classified_rows {
        (rows, None, None)
    } else if let Some(media_id) = media_scope {
        (store.ledger_for_media(media_id).map_err(|error| error.to_string())?, library.map(str::to_owned), folder)
    } else if let Some(library_id) = library {
        (store.ledger_candidates_for_library(&ownership, library_id).map_err(|error| error.to_string())?, Some(library_id.to_string()), None)
    } else if is_folder_id {
        let Some(root) = folder else {
            return Ok(unresolved_scope(ownership, is_admin, user_id));
        };
        let owner = libraries.iter().find(|library| library.root_paths.iter().any(|path| path == &root));
        let rows = match owner {
            Some(library) => store.ledger_candidates_for_library(&ownership, &library.id).map_err(|error| error.to_string())?,
            None => Vec::new(),
        };
        (rows, None, Some(root))
    } else {
        (store.jellyfin_ledger_candidates(&ownership, None).map_err(|error| error.to_string())?, None, None)
    };
    let rows_read = rows.len();
    let ids: Vec<MediaId> = rows.iter().map(|row| row.media_id).collect();
    let media = store.media_batch(&ids).map_err(|error| error.to_string())?;
    let units = store.playback_units_for_media(user_id, &ids).map_err(|error| error.to_string())?;
    let unknown_parent = parent.is_some() && library.is_none() && !is_folder_id && media_target.is_none();
    Ok(ScopedRead { ownership, is_admin, user_id, media, rows, units, requested_library, folder_root, media_scoped: media_scope.is_some(), unresolved: false, unknown_parent, rows_read })
}


pub(super) fn unresolved_scope(ownership: store::LibraryOwnership, is_admin: bool, user_id: UserId) -> ScopedRead {
    ScopedRead { ownership, is_admin, user_id, media: HashMap::new(), rows: Vec::new(), units: Vec::new(), requested_library: None, folder_root: None, media_scoped: false, unresolved: true, unknown_parent: false, rows_read: 0 }
}

fn visible_libraries(
    store: &crate::Store,
    ownership: &store::LibraryOwnership,
    user_id: UserId,
    is_admin: bool,
) -> Result<Vec<crate::store::Library>, String> {
    let allowed: HashSet<String> = ownership
        .libraries()
        .iter()
        .filter(|library| ownership.accepts_work(&library.id))
        .filter(|library| {
            if is_admin {
                library.admin_visible
            } else {
                library.access_mode == "everyone" || library.member_ids.contains(&user_id)
            }
        })
        .map(|library| library.id.clone())
        .collect();
    Ok(store
        .list_libraries()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|library| allowed.contains(&library.id))
        .collect())
}

fn folder_root_in(parent: &str, libraries: &[crate::store::Library]) -> Option<PathBuf> {
    libraries.iter().find_map(|library| library.root_paths.iter().find_map(|root| (folder_id_for_root(root) == parent).then(|| root.clone())))
}

fn series_media_id(store: &crate::Store, user_id: UserId, id: &str, ownership: &store::LibraryOwnership, is_admin: bool) -> Result<(Option<MediaId>, Option<Vec<LedgerRow>>), String> {
    let Some(media_id) = id.parse::<MediaId>().ok() else { return Ok((None, None)); };
    let Some(media) = store.get_media(media_id).map_err(|error| error.to_string())? else { return Ok((None, None)); };
    if media.kind != MediaKind::Tv { return Ok((None, None)); }
    let compact = id.replace('-', "");
    let media_compact = media.id.to_string().replace('-', "");
    if compact != media_compact && id != media.id.to_string() { return Ok((None, None)); }
    let rows = store.ledger_for_media(media.id).map_err(|error| error.to_string())?;
    let visible = rows.iter().any(|row| {
        let Some(owner) = ownership.owner(Path::new(&row.path), media.kind) else { return false; };
        ownership.accepts_work(&owner.id) && if is_admin { owner.admin_visible } else { owner.access_mode == "everyone" || owner.member_ids.contains(&user_id) }
    });
    Ok(visible.then_some(media.id).map(|id| (Some(id), Some(rows))).unwrap_or((None, None)))
}

pub(super) fn in_scope_folder(scope: &ScopedRead, row: &LedgerRow) -> bool {
    scope.folder_root.as_ref().is_none_or(|root| Path::new(&row.path).starts_with(root))
}

pub(super) fn row_visible(scope: &ScopedRead, row: &LedgerRow) -> bool {
    let Some(media) = scope.media.get(&row.media_id) else { return false; };
    let Some(owner) = scope.ownership.owner(Path::new(&row.path), media.kind) else { return false; };
    scope.ownership.accepts_work(&owner.id) && scope.requested_library.as_ref().is_none_or(|library_id| &owner.id == library_id) && if scope.is_admin { owner.admin_visible } else { owner.access_mode == "everyone" || owner.member_ids.contains(&scope.user_id) }
}