//! Delete a Library item: remove ledger rows and files under library roots.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::MediaId;
use serde_json::json;
use std::path::Path as FsPath;
use std::str::FromStr;

use crate::http::{err, ok};
use crate::management::ApiState;

pub(crate) fn _keep() {}

fn remove_physical_ledger_files(rows: &[domain::LedgerRow]) -> (Vec<String>, Vec<String>, u64) {
    let mut removed_paths = Vec::new();
    let mut errors = Vec::new();
    let mut freed_bytes: u64 = 0;
    for row in rows {
        let path = FsPath::new(&row.path);
        match std::fs::metadata(path) {
            Ok(meta) => match std::fs::remove_file(path) {
                Ok(()) => {
                    freed_bytes = freed_bytes.saturating_add(meta.len());
                    removed_paths.push(row.path.clone());
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    removed_paths.push(row.path.clone());
                }
                Err(error) => errors.push(format!("删除文件失败 {}: {error}", row.path)),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                removed_paths.push(row.path.clone())
            }
            Err(error) => errors.push(format!("读取文件失败 {}: {error}", row.path)),
        }
    }
    (removed_paths, errors, freed_bytes)
}

pub(crate) async fn delete_item(
    State(state): State<ApiState>,
    Path((library_id, item_id)): Path<(String, String)>,
) -> Response {
    let media_id = match MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let store = state.store.lock();
    let Some(library) = store.get_library(&library_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "library.missing", "媒体库不存在");
    };
    if !store.library_accepts_work(&library_id).unwrap_or(false) {
        return err(StatusCode::CONFLICT, "library.deleting", "Library 正在删除");
    }
    let Some(media) = store.get_media(media_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "library.missing", "条目不存在");
    };
    let rows = match store.ledger_for_media(media_id) {
        Ok(rows) => rows,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let rows: Vec<domain::LedgerRow> = rows
        .into_iter()
        .filter(|row| {
            crate::http::library::library_for_row_strict(&store, row, &media)
                .is_some_and(|lib| lib.id == library.id)
        })
        .collect();
    if rows.is_empty() {
        return err(
            StatusCode::NOT_FOUND,
            "library.missing",
            "条目不属于该媒体库",
        );
    }

    let (removed_paths, mut errors, freed_bytes) = remove_physical_ledger_files(&rows);
    let mut rows_deleted = 0usize;
    for path in &removed_paths {
        match store.delete_ledger_path(path) {
            Ok(_) => rows_deleted += 1,
            Err(error) => errors.push(format!("删除记录失败 {}: {error}", path)),
        }
    }
    let remaining = store
        .ledger_for_media(media_id)
        .map(|rows| rows.len())
        .unwrap_or(0);
    if remaining == 0 {
        let _ = store.delete_imported_pending_for_media(media_id);
        let _ = store.delete_media_markers_for_media(media_id);
        let _ = store.delete_playback_for_media(media_id);
        let _ = store.delete_collection_items_for_media(&media_id.to_string());
    }
    drop(store);
    if rows_deleted > 0 {
        // ledger 行已删除：该 Library 的浏览投影必须重建。
        state.projection.mark_dirty(&library_id);
    }
    ok(json!({
        "removed_paths": removed_paths,
        "rows_deleted": rows_deleted,
        "freed_bytes": freed_bytes,
        "errors": errors,
    }))
    .into_response()
}
