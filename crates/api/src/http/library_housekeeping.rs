//! 路径核对 + 根目录归并：文件被外部移动后重新关联台账；多根指向同一物理
//! 目录时归并。两者都是「预览 → 应用」两步。
//! FS observations use the HTTP blocking budget after an owned DB snapshot;
//! reconciliation revalidates strict scope, freeze state and source before writes.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::path::Path as FsPath;

use crate::http::{err, ok};
use crate::management::ApiState;

use super::library::missing_library;
use crate::library_reconciliation::index_basenames;

const HOUSEKEEPING_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

struct Reconciliation {
    from: String,
    to: String,
    source: Option<(domain::LedgerRow, Option<domain::MediaKind>)>,
}

fn reconciliation_error(error: crate::blocking_budget::BlockingError) -> Response {
    match error {
        crate::blocking_budget::BlockingError::Busy => {
            err(StatusCode::SERVICE_UNAVAILABLE, "runtime.busy", "服务繁忙，请重试")
        }
        crate::blocking_budget::BlockingError::Timeout => {
            err(StatusCode::GATEWAY_TIMEOUT, "reconciliation.timeout", "目录索引超时")
        }
        crate::blocking_budget::BlockingError::Worker(message) => {
            tracing::error!(%message, "Reconciliation observation worker failed");
            err(StatusCode::INTERNAL_SERVER_ERROR, "runtime.worker", "目录核对失败")
        }
    }
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
        Err(crate::blocking_budget::BlockingError::Busy) => {
            return err(StatusCode::SERVICE_UNAVAILABLE, "runtime.busy", "服务繁忙，请重试");
        }
        Err(crate::blocking_budget::BlockingError::Timeout) => {
            return err(StatusCode::GATEWAY_TIMEOUT, "reconciliation.timeout", "目录索引超时");
        }
        Err(crate::blocking_budget::BlockingError::Worker(message)) => {
            tracing::error!(%message, "目录索引 blocking worker 失败");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "runtime.worker", "目录索引失败");
        }
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
    let (library, ownership, rows) = {
        let store = state.store.lock();
        let Some(library) = store.get_library(&id).ok().flatten() else {
            return missing_library();
        };
        let ownership = match store.library_ownership() {
            Ok(ownership) => ownership,
            Err(error) => return err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error.to_string()),
        };
        let rows = match reconciliation_rows(&store, &body) {
            Ok(rows) => rows,
            Err(error) => return err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error.to_string()),
        };
        (library, ownership, rows)
    };
    let files = state.browse_files.clone();
    let work = state.http_work.run(HOUSEKEEPING_DEADLINE, move || {
        observe_reconciliations(&library, ownership, rows, files.as_ref())
    }).await;
    let (scope, rows) = match work {
        Ok(observed) => observed,
        Err(error) => {
            tracing::warn!(%error, "Reconciliation observation failed");
            return reconciliation_error(error);
        }
    };
    let mut reconciled = 0usize;
    let mut errors = Vec::new();
    let store = state.store.lock();
    let Some(current_library) = store.get_library(&id).ok().flatten() else {
        return missing_library();
    };
    let current = match store.library_ownership() {
        Ok(current) => current,
        Err(error) => return err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error.to_string()),
    };
    let valid = scope.as_ref().is_some_and(|scope| {
        scope.matches(&current) && scope.accepts_work(&id) && current.accepts_work(&id)
    });
    for row in rows {
        let row = match row {
            Ok(row) => row,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        if !valid {
            tracing::warn!(library_id = %id, "Reconciliation scope changed or frozen");
            errors.push(format!("{} 不属于当前媒体库范围", row.to));
            continue;
        }
        match apply_reconciliation(&store, &current_library, &row) {
            Ok(()) => reconciled += 1,
            Err(error) => errors.push(error),
        }
    }
    tracing::debug!(library_id = %id, reconciled, errors = errors.len(), "Reconciliation applied");
    ok(json!({ "reconciled": reconciled, "errors": errors })).into_response()
}

fn reconciliation_rows(store: &store::Store, body: &Value) -> Result<Vec<Reconciliation>, store::StoreError> {
    let mut rows = Vec::new();
    if let Some(input) = body["reconciliations"].as_array() {
        for row in input {
            let (Some(from), Some(to)) = (row["from"].as_str(), row["to"].as_str()) else {
                continue;
            };
            let source = match store.ledger_by_path(from)? {
                Some(row) => {
                    let kind = store.get_media(row.media_id)?.map(|media| media.kind);
                    Some((row, kind))
                }
                None => None,
            };
            rows.push(Reconciliation { from: from.to_owned(), to: to.to_owned(), source });
        }
    }
    Ok(rows)
}

fn observe_reconciliations(
    library: &store::Library,
    ownership: store::LibraryOwnership,
    rows: Vec<Reconciliation>,
    files: &dyn crate::browse_file_read::BrowseFileRead,
) -> (Option<store::LibraryPathScope>, Vec<Result<Reconciliation, String>>) {
    let scope = match store::LibraryPathScope::resolve(ownership) {
        Ok(scope) => Some(scope),
        Err(error) => {
            tracing::warn!(%error, "Reconciliation strict scope resolution failed");
            None
        }
    };
    let observed = rows.into_iter().map(|row| {
        if FsPath::new(&row.from).exists() {
            return Err(format!("{}: 原文件仍然存在，不能重关联", row.from));
        }
        if !files.is_file(FsPath::new(&row.to)) {
            return Err(format!("{} 不是文件", row.to));
        }
        if !strictly_owned(scope.as_ref(), FsPath::new(&row.to), library.kind, &library.id) {
            return Err(format!("{} 不属于当前媒体库范围", row.to));
        }
        let Some((source, kind)) = row.source.as_ref() else {
            return Err(format!("{} 不在台账中", row.from));
        };
        if !kind.is_some_and(|kind| strictly_owned(scope.as_ref(), FsPath::new(&source.path), kind, &library.id)) {
            return Err(format!("{} 不属于当前媒体库", row.from));
        }
        Ok(row)
    }).collect();
    (scope, observed)
}

fn strictly_owned(scope: Option<&store::LibraryPathScope>, path: &FsPath, kind: domain::MediaKind, id: &str) -> bool {
    let Some(scope) = scope else { return false };
    match store::LibraryPathScope::resolve_path(path) {
        Ok(path) => matches!(scope.owner(&path, kind), store::StrictOwner::Owned(owner) if owner.id == id),
        Err(error) => {
            tracing::warn!(%error, "Reconciliation path resolution failed");
            false
        }
    }
}

fn apply_reconciliation(store: &store::Store, library: &store::Library, row: &Reconciliation) -> Result<(), String> {
    let failure = || format!("{} -> {} 台账重命名失败", row.from, row.to);
    let Some((source, kind)) = row.source.as_ref() else { return Err(failure()) };
    let current = store.ledger_by_path(&row.from).map_err(|error| {
        tracing::warn!(%error, "Reconciliation source revalidation failed");
        failure()
    })?;
    let media = store.get_media(source.media_id).map_err(|error| {
        tracing::warn!(%error, "Reconciliation media revalidation failed");
        failure()
    })?;
    if current.as_ref() != Some(source) || media.as_ref().map(|media| media.kind) != *kind {
        return Err(failure());
    }
    match store.rename_ledger_path(&row.from, &row.to) {
        Ok(true) => {
            if let Err(error) = store.rewrite_fact_paths(source.media_id, &row.from, &row.to) {
                tracing::error!(library_id = %library.id, %error, "Reconciliation fact path rewrite failed");
            }
            Ok(())
        }
        result => {
            tracing::warn!(library_id = %library.id, ?result, "Reconciliation ledger rename failed");
            Err(failure())
        }
    }
}

/// POST /libraries/{id}/root-consolidation-preview — 物理目录去重。
pub(crate) async fn root_consolidation_preview(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let library = {
        let store = state.store.lock();
        let Some(library) = store.get_library(&id).ok().flatten() else {
            return missing_library();
        };
        library
    };
    let result = state.http_work.run(HOUSEKEEPING_DEADLINE, move || {
        let mut seen: Vec<(String, String)> = Vec::new(); // (canonical, path)
        let mut duplicates = Vec::new();
        for root in &library.root_paths {
            let Ok(real) = std::fs::canonicalize(root) else {
                continue;
            };
            let real = real.display().to_string();
            if let Some((keep, _)) = seen.iter().find(|(c, _)| *c == real) {
                duplicates.push(json!({ "keep": keep, "merge": root }));
            } else {
                seen.push((real, root.display().to_string()));
            }
        }
        duplicates
    }).await;
    match result {
        Ok(duplicates) => ok(json!({ "duplicates": duplicates })).into_response(),
        Err(error) => {
            tracing::warn!(%error, "Root consolidation observation failed");
            reconciliation_error(error)
        }
    }
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
