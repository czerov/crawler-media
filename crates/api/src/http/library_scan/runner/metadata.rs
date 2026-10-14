//! chunk 内 Media 身份解析与元数据/刮削落盘。
//!
//! 入账（ledger）先于这里的解析：解析失败时调用方必须把已提交前缀计入失败报告（P3-3）。

use std::collections::HashMap;
use std::path::PathBuf;

use axum::http::StatusCode;
use axum::response::Response;

use crate::http::err;
use crate::http::library::rows_in_library;
use crate::management::ApiState;

use super::super::{ledger_row_by_path, library_by_id, media_by_id, scrape_library_metadata};

/// chunk 内涉及到的非 Video Media 台账行（身份解析按 Media 去重）。
pub(super) fn scan_media_for(
    state: &ApiState,
    files: &[library::TransferredFile],
) -> HashMap<domain::MediaId, (domain::Media, domain::LedgerRow)> {
    let store = state.store.lock();
    let mut map = HashMap::new();
    for file in files {
        let Some(row) = ledger_row_by_path(&store, &file.path.display().to_string()) else {
            continue;
        };
        let Some(m) = media_by_id(&store, row.media_id) else {
            continue;
        };
        if m.kind != domain::MediaKind::Video {
            map.entry(m.id).or_insert((m, row));
        }
    }
    map
}

/// 对 chunk 内已入账的 Media 做身份收敛与刮削（NFO / 图片）。
pub(super) fn resolve_scan_metadata(
    state: &ApiState,
    id: &str,
    lease: &crate::library_lifecycle::LibraryWorkLease,
    scan_media: HashMap<domain::MediaId, (domain::Media, domain::LedgerRow)>,
    inserted: &[PathBuf],
    scrape: bool,
) -> Result<(), Response> {
    for (_, (media, row)) in scan_media {
        if lease.control().is_cancelled() {
            return Err(err(
                StatusCode::CONFLICT,
                "library.deleting",
                "Library 正在删除",
            ));
        }
        let path_buf = PathBuf::from(&row.path);
        let path = path_buf.as_path();
        if crate::auto_resolve::has_local_scan_metadata(state, &media, path) {
            continue;
        }
        let resolved = crate::auto_resolve::resolve_scanned_media(state, &media, path)
            .unwrap_or_else(|| media.clone());
        if let Err(error) =
            crate::auto_resolve::rebind_scan_rows(state, media.id, resolved.id, inserted)
        {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error,
            ));
        }
        if scrape {
            // 见上：真实失败由 poster_fetch 内部记录，None 不是错误。
            let _ = crate::poster_fetch::attach_poster(state, &resolved, path);
            let _ = crate::poster_fetch::attach_backdrop(state, &resolved, path);
            // Reread after alias convergence; stale rows still contain the provisional ID.
            let (row, rows) = {
                let store = state.store.lock();
                let row = ledger_row_by_path(&store, &row.path).unwrap_or(row);
                let rows = library_by_id(&store, id)
                    .map(|library| rows_in_library(&store, &library))
                    .unwrap_or_default();
                (row, rows)
            };
            scrape_library_metadata(state, &resolved, &row, path, &rows);
        }
    }

    Ok(())
}
