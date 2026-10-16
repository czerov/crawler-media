//! 路径核对 + 根目录归并：文件被外部移动后重新关联台账；多根指向同一物理
//! 目录时归并。两者都是「预览 → 应用」两步。

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::path::{Path as FsPath, PathBuf};

use crate::http::{err, ok};
use crate::management::ApiState;

use super::library::missing_library;
use crate::library_reconciliation::index_basenames;


fn canonical(path: &FsPath) -> Option<PathBuf> {
    fs_canonicalize(path).ok()
}

// 独立函数便于测试与 future 注入。
fn fs_canonicalize(path: &FsPath) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path)
}


/// POST /libraries/{id}/path-reconciliation-preview — 台账失联行 + 库内新候选。
pub(crate) async fn path_reconciliation_preview(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let (library, rows) = {
        let store = state.store.lock();
        let Some(library) = store.get_library(&id).ok().flatten() else {
            return missing_library();
        };
        let rows = super::library::rows_in_library(&store, &library);
        (library, rows)
    };

    let roots = library.root_paths.clone();
    let control = library::WorkControl::default();

    let preview_res = state
        .http_work
        .run(std::time::Duration::from_secs(30), move || -> Result<(Vec<serde_json::Value>, usize, bool), String> {
            let index = index_basenames(&roots, &control, 100_000)?;
            let mut missing = Vec::new();
            for row in rows {
                let row_path = FsPath::new(&row.path);
                if row_path.is_file() {
                    continue;
                }
                let name_os = row_path.file_name().unwrap_or_default();
                let name_str = name_os.to_str().unwrap_or_default().to_string();

                let mut candidates = Vec::new();
                if let Some(matches) = index.candidates.get(name_os) {
                    for cand in matches {
                        if cand != row_path {
                            candidates.push(cand.display().to_string());
                        }
                    }
                }

                missing.push(json!({
                    "file_id": row.id.to_string(),
                    "media_id": row.media_id.to_string(),
                    "path": row.path,
                    "file_name": name_str,
                    "candidates": candidates,
                }));
            }
            Ok((missing, index.examined, index.incomplete))
        })
        .await;

    let (missing, examined, incomplete) = match preview_res {
        Ok(Ok(data)) => data,
        Ok(Err(e)) => return err(StatusCode::INTERNAL_SERVER_ERROR, "reconciliation.index_failed", &e),
        Err(_) => return err(StatusCode::REQUEST_TIMEOUT, "reconciliation.timeout", "目录索引超时"),
    };

    ok(json!({
        "missing": missing,
        "examined": examined,
        "incomplete": incomplete,
    }))
    .into_response()
}

/// POST /libraries/{id}/path-reconciliations — 应用重关联。
pub(crate) async fn path_reconciliations(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let store = state.store.lock();
    let Some(library) = store.get_library(&id).ok().flatten() else {
        return crate::http::library::missing_library();
    };
    let mut reconciled = 0usize;
    let mut errors = Vec::new();
    if let Some(rows) = body["reconciliations"].as_array() {
        for row in rows {
            let (Some(from), Some(to)) = (row["from"].as_str(), row["to"].as_str()) else {
                continue;
            };
            let to_path = FsPath::new(to);
            let from_path = FsPath::new(from);
            if from_path.exists() {
                errors.push(format!("{from}: 原文件仍然存在，不能重关联"));
                continue;
            }
            if !to_path.is_file() {
                errors.push(format!("{to} 不是文件"));
                continue;
            }
            // 目标路径与源路径必须都在当前媒体库严格边界范围内
            let to_owner = store
                .library_for_path_strict(to_path, library.kind)
                .ok()
                .flatten();
            if to_owner.as_ref().map(|l| &l.id) != Some(&library.id) {
                errors.push(format!("{to} 不属于当前媒体库范围"));
                continue;
            }
            let Some(from_row) = store.ledger_by_path(from).ok().flatten() else {
                errors.push(format!("{from} 不在台账中"));
                continue;
            };
            let from_owner = store
                .get_media(from_row.media_id)
                .ok()
                .flatten()
                .and_then(|media| {
                    store
                        .library_for_path_strict(FsPath::new(&from_row.path), media.kind)
                        .ok()
                        .flatten()
                });
            if from_owner.as_ref().map(|l| &l.id) != Some(&library.id) {
                errors.push(format!("{from} 不属于当前媒体库"));
                continue;
            }
            if store.rename_ledger_path(from, to).unwrap_or(false) {
                // facts 路径同步（洗版替换仍能找到旧位置）。
                if let Some(ledger_row) = store
                    .list_ledger()
                    .unwrap_or_default()
                    .into_iter()
                    .find(|r| r.path == to)
                {
                    let _ = store.rewrite_fact_paths(ledger_row.media_id, from, to);
                }
                reconciled += 1;
            } else {
                errors.push(format!("{from} -> {to} 台账重命名失败"));
            }
        }
    }
    ok(json!({ "reconciled": reconciled, "errors": errors })).into_response()
}

/// POST /libraries/{id}/root-consolidation-preview — 物理目录去重。
pub(crate) async fn root_consolidation_preview(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let store = state.store.lock();
    let Some(library) = store.get_library(&id).ok().flatten() else {
        return missing_library();
    };
    let mut seen: Vec<(String, String)> = Vec::new(); // (canonical, path)
    let mut duplicates = Vec::new();
    for root in &library.root_paths {
        let Some(real) = canonical(&root) else {
            continue;
        };
        let real = real.display().to_string();
        if let Some((keep, _)) = seen.iter().find(|(c, _)| *c == real) {
            duplicates.push(json!({ "keep": keep, "merge": root }));
        } else {
            seen.push((real, root.display().to_string()));
        }
    }
    ok(json!({ "duplicates": duplicates })).into_response()
}

/// POST /libraries/{id}/root-consolidations — 归并（删重复根，保留第一个）。
pub(crate) async fn root_consolidations(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let store = state.store.lock();
    let Some(library) = store.get_library(&id).ok().flatten() else {
        return missing_library();
    };
    let Some(roots) = body["roots"].as_array() else {
        return err(StatusCode::BAD_REQUEST, "consolidate.invalid", "缺少 roots");
    };
    let to_remove: Vec<String> = roots
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let remaining: Vec<String> = library
        .root_paths
        .iter()
        .filter(|root| !to_remove.contains(&root.display().to_string()))
        .map(|root| root.display().to_string())
        .collect();
    let refs: Vec<&str> = remaining.iter().map(String::as_str).collect();
    match store.update_library(&id, None, Some(&refs), None) {
        Ok(_) => ok(json!({
            "consolidated": to_remove.len(),
            "roots": remaining,
        }))
        .into_response(),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}
