//! Library admin views: raw ledger rows and unidentified files.
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{Confidence, MediaId, QualitySource};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::str::FromStr;

use crate::http::{err, ok, ok_list};
use crate::management::ApiState;

/// GET /ledger — every ledger row (management view).
pub(crate) async fn list_ledger(State(state): State<ApiState>) -> Response {
    let store = state.store.lock();
    let rows_with_mode = match store.list_ledger_with_mode() {
        Ok(rows) => rows,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let global_transfer_mode = store.transfer_mode().unwrap_or_else(|_| "hardlink".into());
    let items: Vec<Value> = rows_with_mode
        .into_iter()
        .map(|(row, saved_mode, source_path)| {
            let media = store.get_media(row.media_id).ok().flatten();
            let library_id = media.as_ref().and_then(|media| {
                crate::http::library::library_for_row(&store, &row, media).map(|library| library.id)
            });
            let title = media.as_ref().map(|m| m.title.clone()).unwrap_or_default();
            let kind = media.as_ref().map(|m| m.kind.as_str()).unwrap_or("movie");
            let meta = std::fs::metadata(&row.path).ok();
            let file_size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let is_strm = std::path::Path::new(&row.path)
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("strm"));

            #[cfg(unix)]
            let is_hardlink = {
                use std::os::unix::fs::MetadataExt;
                meta.as_ref().map(|m| m.nlink() > 1).unwrap_or(false)
            };
            #[cfg(not(unix))]
            let is_hardlink =
                saved_mode.as_deref() == Some("hardlink") || global_transfer_mode == "hardlink";

            let mode_str = if is_strm {
                "strm".to_string()
            } else if is_hardlink {
                "hardlink".to_string()
            } else {
                saved_mode.unwrap_or_else(|| global_transfer_mode.clone())
            };

            json!({
                "id": row.id.to_string(),
                "media_id": row.media_id.to_string(),
                "library_id": library_id,
                "media_title": title,
                "media_kind": kind,
                "path": row.path,
                "source_path": source_path,
                "season": row.season,
                "episode": row.episode,
                "resolution": row.resolution,
                "codec": row.codec,
                "hdr": row.hdr,
                "quality_source": row.quality_source.as_str(),
                "confidence": row.confidence.as_str(),
                "filter_score": row.filter_score,
                "transfer_mode": mode_str,
                "file_size": file_size,
            })
        })
        .collect();
    ok_list(items).into_response()
}
#[derive(serde::Deserialize)]
pub(crate) struct DeleteLedgerQuery {
    delete_file: Option<bool>,
}

/// DELETE /ledger/{id} — delete a ledger row and optionally remove the file on disk.
pub(crate) async fn delete_ledger_row(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(query): Query<DeleteLedgerQuery>,
) -> Response {
    let ledger_id = match domain::LedgerId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "ledger.invalid", "ledger id 无效"),
    };
    let store = state.store.lock();
    let rows = match store.list_ledger() {
        Ok(rows) => rows,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let Some(target) = rows.into_iter().find(|r| r.id == ledger_id) else {
        return err(StatusCode::NOT_FOUND, "ledger.missing", "整理记录不存在");
    };
    if let Some(media) = store.get_media(target.media_id).ok().flatten() {
        if crate::http::library::library_for_row_strict(&store, &target, &media)
            .is_some_and(|owner| !store.library_accepts_work(&owner.id).unwrap_or(false)) {
            return err(StatusCode::CONFLICT, "library.deleting", "Library 正在删除");
        }
    }
    let delete_file = query.delete_file.unwrap_or(false);
    if delete_file {
        let owned = store
            .get_media(target.media_id)
            .ok()
            .flatten()
            .and_then(|media| {
                crate::http::library::library_for_row_strict(&store, &target, &media)
            });
        if owned.is_none() {
            return err(
                StatusCode::FORBIDDEN,
                "library.outside_root",
                "文件已不在任何媒体库根目录内，拒绝物理删除",
            );
        }
    }
    if delete_file {
        if let Err(e) = std::fs::remove_file(&target.path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "file.delete_failed",
                    &e.to_string(),
                );
            }
        }
    }
    if let Err(error) = store.delete_ledger_path(&target.path) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    ok(json!({ "deleted": true, "path": target.path, "file_deleted": delete_file })).into_response()
}

/// GET /unidentified — files that failed confidence matching.
pub(crate) async fn list_unidentified(
    State(state): State<ApiState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let store = state.store.lock();
    let rows = match store.list_unidentified() {
        Ok(rows) => rows,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let target_lib_id = query.get("library_id").map(|s| s.as_str());
    let mut items: Vec<Value> = Vec::new();

    for (path, confidence) in rows {
        let fspath = std::path::Path::new(&path);
        let lib_match = store
            .library_for_path(fspath, domain::MediaKind::Movie)
            .ok()
            .flatten()
            .or_else(|| {
                store
                    .library_for_path(fspath, domain::MediaKind::Tv)
                    .ok()
                    .flatten()
            });
        let lib_id = lib_match.as_ref().map(|l| l.id.clone()).unwrap_or_default();
        let lib_name = lib_match
            .as_ref()
            .map(|l| l.name.clone())
            .unwrap_or_default();
        if let Some(target_id) = target_lib_id {
            if lib_id != target_id {
                continue;
            }
        }
        let file_name = fspath
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&path)
            .to_string();
        items.push(json!({
            "key": path.clone(),
            "path": path.clone(),
            "confidence": confidence,
            "label": file_name,
            "library_id": lib_id,
            "library_name": lib_name,
            "file_count": 1,
            "total_size_bytes": std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
            "files": [{
                "file_id": path.clone(),
                "file_path": path,
            }],
        }));
    }
    ok_list(items).into_response()
}

/// POST /ledger/{id}/retransfer — 从源地址重新硬链接/重新生成文件到媒体库
pub(crate) async fn retransfer_ledger_row(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let ledger_id = match domain::LedgerId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "ledger.invalid", "ledger id 无效"),
    };
    let (destination, src) = {
        let store = state.store.lock();
        let rows_with_mode = match store.list_ledger_with_mode() {
            Ok(rows) => rows,
            Err(error) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        };
        let Some((target, _, source_path)) = rows_with_mode
            .into_iter()
            .find(|(r, _, _)| r.id == ledger_id)
        else {
            return err(StatusCode::NOT_FOUND, "ledger.missing", "整理记录不存在");
        };
        let Some(src) = source_path else {
            return err(
                StatusCode::BAD_REQUEST,
                "ledger.no_source",
                "该条目未记录源文件地址，无法重新生成",
            );
        };
        let Some(media) = store.get_media(target.media_id).ok().flatten() else {
            return err(
                StatusCode::FORBIDDEN,
                "library.outside_root",
                "无法确认媒体库归属，拒绝重新生成",
            );
        };
        if crate::http::library::library_for_row_strict(&store, &target, &media).is_none() {
            return err(
                StatusCode::FORBIDDEN,
                "library.outside_root",
                "目标已不在任何媒体库根目录内，拒绝重新生成",
            );
        }
        (target.path, src)
    };
    let src_path = std::path::PathBuf::from(&src);
    let dest_path = std::path::PathBuf::from(&destination);
    if !src_path.is_file() {
        return err(
            StatusCode::BAD_REQUEST,
            "ledger.source_not_found",
            &format!("源下载文件已不存在: {src}"),
        );
    }
    if dest_path.exists() {
        return err(
            StatusCode::CONFLICT,
            "ledger.destination_exists",
            "媒体库目标文件仍存在，无需重新生成",
        );
    }
    let source_for_task = src_path.clone();
    let destination_for_task = dest_path.clone();
    let transfer = tokio::task::spawn_blocking(move || {
        library::transfer_file(
            &source_for_task,
            &destination_for_task,
            library::TransferMode::Hardlink,
        )
        .map_err(|error| error.to_string())
    })
    .await;
    let transfer = match transfer {
        Ok(result) => result,
        Err(error) => {
            tracing::error!(%error, "媒体库重新转移任务失败");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "transfer.failed",
                &error.to_string(),
            );
        }
    };
    if let Err(error) = transfer {
        tracing::error!(
            source = %src_path.display(),
            destination = %dest_path.display(),
            %error,
            "无法从其记录的源路径重新转移媒体库文件"
        );
        return err(StatusCode::INTERNAL_SERVER_ERROR, "transfer.failed", &error);
    }
    tracing::info!(
        source = %src_path.display(),
        destination = %dest_path.display(),
        "已从其记录的源路径重新转移媒体库文件"
    );

    if let Err(error) = state.store.lock().clear_ledger_missing(&destination) {
        tracing::error!(
            destination = %destination,
            %error,
            "重新转移后无法清除缺失标记"
        );
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }

    ok(json!({
        "retransferred": true,
        "source": src,
        "destination": destination
    }))
    .into_response()
}
pub(crate) async fn claim_unidentified(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let path = body["path"].as_str().unwrap_or_default().trim();
    if path.is_empty() {
        return err(StatusCode::BAD_REQUEST, "library.invalid", "path 必填");
    }
    let media_id = match MediaId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "media id 无效"),
    };
    let store = state.store.lock();
    let Some(media) = store.get_media(media_id).ok().flatten() else {
        return err(
            StatusCode::NOT_FOUND,
            "library.item_missing",
            "影视条目不存在",
        );
    };
    // 只登记确实在某个 Library 根下的文件。任意路径写成台账后，订阅删除
    // 会把它当成库内文件物理删掉。
    if store
        .library_for_path_strict(std::path::Path::new(path), media.kind)
        .ok()
        .flatten()
        .is_none()
    {
        return err(
            StatusCode::BAD_REQUEST,
            "library.path_outside",
            "只能认领媒体库根目录内的文件",
        );
    }
    let row = domain::LedgerRow {
        id: domain::LedgerId::new(),
        media_id,
        path: path.to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    if let Err(error) = store.insert_ledger(&row) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    let _ = store.delete_unidentified(path);
    drop(store);
    crate::http::library::enqueue_probes_for_rows(&state, std::slice::from_ref(&row));
    ok(json!({
        "media_item_id": media.id.to_string(),
        "path": path,
    }))
    .into_response()
}
