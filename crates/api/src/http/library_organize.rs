//! Organize: rename existing library files per the effective naming template
//! (preview then apply), plus single-item metadata refresh and the per-library
//! image gallery.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::path::{Path as FsPath, PathBuf};
use std::str::FromStr;

use crate::blocking_budget::BlockingError;
use crate::http::library::{
    library_for_row, preferred_row, require_visible_library, rows_in_library,
};
use crate::http::{err, ok, ok_list};
use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;

use super::library::missing_library;

/// 只读整理预览/放宽建议/相似推荐的等待预算。
const ORGANIZE_READ_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// Blocking-budget failure → HTTP response（上游/存储错误走各自原有路径）。
fn blocking_error(error: BlockingError, code: &'static str) -> Response {
    match error {
        BlockingError::Busy => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请重试",
        ),
        BlockingError::Timeout => err(StatusCode::GATEWAY_TIMEOUT, code, "整理请求超时"),
        error => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "runtime.worker",
            &error.to_string(),
        ),
    }
}

fn release_for(row: &domain::LedgerRow) -> domain::Release {
    let name = FsPath::new(&row.path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let mut parsed = release::parse(name);
    parsed.resolution = row.resolution.clone().or(parsed.resolution);
    parsed.codec = row.codec.clone().or(parsed.codec);
    parsed.hdr = row.hdr.clone().or(parsed.hdr);
    parsed
}

/// GET /libraries/{id}/organize-preview — target names for every owned file
/// under the library's naming template.
pub(crate) async fn preview_organize(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let state_for_work = state.clone();
    let work = state
        .http_work
        .run(ORGANIZE_READ_DEADLINE, move || {
            preview_organize_inner(&state_for_work, &id)
        })
        .await;
    match work {
        Ok(response) => response,
        Err(error) => blocking_error(error, "organize.timeout"),
    }
}

fn preview_organize_inner(state: &ApiState, id: &str) -> Response {
    let store = state.store.lock();
    let Some(library) = store.get_library(id).ok().flatten() else {
        return missing_library();
    };
    let pattern = match store.naming_pattern(library.kind) {
        Ok(pattern) => pattern,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let primary = library.root_paths.first().cloned().unwrap_or_default();
    let mut renames = Vec::new();
    let mut already_ok = 0usize;
    let mut skips = 0usize;
    for row in rows_in_library(&store, &library) {
        let Some(media) = store.get_media(row.media_id).ok().flatten() else {
            continue;
        };
        let release = release_for(&row);
        let target = match library::render_path(
            &primary,
            &pattern,
            &media,
            &release,
            FsPath::new(&row.path),
        ) {
            Ok(target) => target,
            Err(_) => {
                skips += 1;
                continue;
            }
        };
        if target.to_string_lossy() == row.path {
            already_ok += 1;
            continue;
        }
        renames.push(json!({
            "from": row.path,
            "to": target.display().to_string(),
            "title": media.title,
            "kind": media.kind.as_str(),
        }));
    }
    ok(json!({
        "total": renames.len() + already_ok + skips,
        "already_ok": already_ok,
        "skips": skips,
        "renames": renames,
    }))
    .into_response()
}

/// POST /libraries/{id}/organize — apply the previewed renames. Body:
/// `{"renames": [{"from","to"}, ...]}`.
pub(crate) async fn organize(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let state_for_work = state.clone();
    let work = state
        .http_work
        .run_to_completion(move || organize_inner(&state_for_work, &id, &body))
        .await;
    match work {
        Ok(response) => response,
        Err(error) => blocking_error(error, "organize.worker"),
    }
}

/// 整个同步整理用例（lease → 校验 → 落盘）都在 blocking 闭包内，
/// LibraryLease 持有到真实工作结束。
fn organize_inner(state: &ApiState, id: &str, body: &Value) -> Response {
    let lease = match state.acquire_library_work(id) {
        Ok(lease) => lease,
        Err(error) => return err(StatusCode::CONFLICT, "library.frozen", &error),
    };
    let store = state.store.lock();
    let Some(library) = store.get_library(id).ok().flatten() else {
        return missing_library();
    };
    let Some(renames) = body["renames"].as_array() else {
        return err(
            StatusCode::BAD_REQUEST,
            "organize.invalid",
            "缺少 renames 列表",
        );
    };
    match lease
        .control()
        .signal()
        .publish(|| Ok(apply_organize(&store, &library, renames)))
    {
        Ok(response) => response,
        Err(error) => err(StatusCode::CONFLICT, "library.frozen", &error),
    }
}

fn apply_organize(store: &crate::Store, library: &store::Library, renames: &[Value]) -> Response {
    let mut applied = 0usize;
    let mut skipped = Vec::new();
    let mut errors = Vec::new();
    for entry in renames {
        let (Some(from), Some(to)) = (entry["from"].as_str(), entry["to"].as_str()) else {
            continue;
        };
        let to_path = FsPath::new(to);
        if to_path == FsPath::new(from) {
            applied += 1;
            continue;
        }
        let from_path = FsPath::new(from);
        let from_owner = store
            .library_for_path_strict(from_path, library.kind)
            .ok()
            .flatten();
        if from_owner.as_ref().map(|l| &l.id) != Some(&library.id) {
            errors.push(format!("{from}: 源文件不属于当前媒体库"));
            continue;
        }
        let to_owner = store
            .library_for_path_strict(to_path, library.kind)
            .ok()
            .flatten();
        if to_owner.as_ref().map(|l| &l.id) != Some(&library.id) {
            errors.push(format!("{to}: 目标路径不属于当前媒体库范围"));
            continue;
        }
        if !from_path.is_file() {
            skipped.push(format!("源文件不存在 {from}"));
            continue;
        }
        if to_path.exists() {
            skipped.push(format!("目标已存在 {to}"));
            continue;
        }
        // 预检目标路径在 ledger 中是否已被占用（避免改名后因 UNIQUE 约束造成 ledger 记录孤立和归属错乱）
        if store.ledger_by_path(to).ok().flatten().is_some() {
            errors.push(format!("{to}: 目标路径已在台账中存在"));
            continue;
        }
        // 源文件必须已登记在当前媒体库的台账中，防止整理未登记的任意宿主机文件
        let Some(source_ledger) = store.ledger_by_path(from).ok().flatten() else {
            errors.push(format!("{from}: 源文件未登记在台账中"));
            continue;
        };
        let Some(source_media) = store.get_media(source_ledger.media_id).ok().flatten() else {
            errors.push(format!("{from}: 关联媒体信息不存在"));
            continue;
        };
        if crate::http::library::library_for_row(&store, &source_ledger, &source_media)
            .as_ref()
            .map(|l| &l.id)
            != Some(&library.id)
        {
            errors.push(format!("{from}: 台账记录不属于当前媒体库"));
            continue;
        }
        if let Some(parent) = to_path.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                tracing::error!(from, to, %error, "创建整理目标目录失败");
                errors.push(format!("{from}: {error}"));
                continue;
            }
        }
        if let Err(error) = std::fs::rename(from, to) {
            tracing::error!(from, to, %error, "整理文件重命名失败");
            errors.push(format!("{from} 重命名失败: {error}"));
            continue;
        }
        if let Err(err) = store.rename_ledger_path(from, to) {
            tracing::error!(%err, from, to, "整理文件更新台账失败，尝试回滚磁盘");
            if let Err(rollback_err) = std::fs::rename(to, from) {
                tracing::error!(%rollback_err, from, to, "整理文件回滚重命名失败");
            }
            errors.push(format!("{from} -> {to}: 台账记录更新失败: {err}"));
            continue;
        }
        // 重命名后重写分集 NFO（新 stem）：扫描只给新文件写 NFO，已入台账的
        // 文件整理后必须跟着换名，否则 .nfo 变成孤儿。
        if let Ok(Some(ledger_row)) = store.ledger_by_path(to) {
            if let Ok(Some(media_row)) = store.get_media(ledger_row.media_id) {
                if let Some(new_stem) = FsPath::new(to).file_stem().and_then(|n| n.to_str()) {
                    let nfo_path = FsPath::new(to).with_file_name(format!("{new_stem}.nfo"));
                    if !nfo_path.is_file() {
                        let _ = library::write_nfo(&nfo_path, &media_row, None);
                    }
                }
            }
        }
        // 归属订阅的事实路径同步（洗版替换仍能找到旧文件的位置）。
        if let Some(row) = store
            .list_ledger()
            .unwrap_or_default()
            .into_iter()
            .find(|row| row.path == to)
        {
            let _ = store.rewrite_fact_paths(row.media_id, from, to);
        }
        applied += 1;
    }
    let _ = library;
    ok(json!({
        "applied": applied,
        "skipped": skipped,
        "errors": errors,
    }))
    .into_response()
}

/// POST /libraries/{id}/items/{item_id}/metadata/refresh — single item:
/// poster + backdrop + rich NFO from TMDB.
pub(crate) async fn refresh_item_metadata(
    State(state): State<ApiState>,
    Path((id, item_id)): Path<(String, String)>,
) -> Response {
    let state_for_work = state.clone();
    let work = state
        .http_work
        .run_to_completion(move || {
            refresh_item_metadata_inner(&state_for_work, &id, &item_id)
        })
        .await;
    match work {
        Ok(response) => response,
        Err(error) => blocking_error(error, "organize.worker"),
    }
}

/// 整个同步刷新用例（lease → TMDB 抓取 → 海报/NFO 落盘）在 blocking 闭包内，
/// LibraryLease 持有到真实工作结束。
fn refresh_item_metadata_inner(state: &ApiState, id: &str, item_id: &str) -> Response {
    let lease = match state.acquire_library_work(id) {
        Ok(lease) => lease,
        Err(error) => return err(StatusCode::CONFLICT, "library.frozen", &error),
    };
    let store = state.store.lock();
    let Some(library) = store.get_library(id).ok().flatten() else {
        return missing_library();
    };
    let media_id = match domain::MediaId::from_str(item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let Some(media) = store.get_media(media_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
    };
    let rows: Vec<domain::LedgerRow> = rows_in_library(&store, &library)
        .into_iter()
        .filter(|row| row.media_id == media_id)
        .collect();
    let Some(row) = preferred_row(&rows).cloned() else {
        return err(
            StatusCode::NOT_FOUND,
            "library.item_missing",
            "条目没有在位文件",
        );
    };
    drop(store);
    let path = FsPath::new(&row.path).to_path_buf();
    // 剧集级目录：模板整理后是 `<show>/Season N/file`，show 根 = 文件父目录的上一级；
    // 平铺时 = 文件所在目录。poster/fanart/tvshow.nfo 落在 show 根。
    let file_dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let show_root: PathBuf = if media.kind == domain::MediaKind::Tv {
        crate::scrape_metadata::show_root(&path, &row)
    } else {
        file_dir.clone()
    };
    let mut refreshed = 0;
    if let Some(bytes) = crate::poster_fetch::attach_poster(&state, &media, &path) {
        refreshed += 1;
        let _ = lease.control().signal().publish(|| {
            std::fs::write(show_root.join("poster.jpg"), &bytes).map_err(|error| error.to_string())
        });
    }
    if let Some(bytes) = crate::poster_fetch::force_attach_backdrop(&state, &media, &path) {
        let _ = lease.control().signal().publish(|| {
            std::fs::write(show_root.join("fanart.jpg"), &bytes).map_err(|error| error.to_string())
        });
    }
    if let Some(tmdb_id) = media.tmdb_id.as_deref() {
        match crate::scrape_metadata::fetch_tmdb_metadata(&state, media.kind, tmdb_id) {
            Ok(Some(meta)) => {
                let nfo = crate::scrape_metadata::nfo_from_tmdb(&media, &meta);
                if media.kind == domain::MediaKind::Tv {
                    crate::scrape_metadata::scrape_tv_sidecars(
                        &state,
                        &media,
                        &show_root,
                        &rows,
                    );
                } else if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
                    let target = path.with_file_name(format!("{stem}.nfo"));
                    let _ = lease.control().signal().publish(|| {
                        crate::scrape_metadata::write_nfo(&target, &media, &nfo);
                        Ok(())
                    });
                    let movie_root = path.parent().unwrap_or(&path);
                    crate::scrape_metadata::scrape_movie_fanart(&state, &media, movie_root);
                }
            }
            Ok(None) => {
                tracing::debug!(%tmdb_id, "catalog returned no metadata for library item refresh")
            }
            Err(error) => {
                tracing::warn!(%error, %tmdb_id, "failed to refresh library item metadata")
            }
        }
    }
    ok(json!({ "refreshed": refreshed })).into_response()
}

/// POST /libraries/{id}/items/relax-filter — 筛空时的放宽建议：对每个已激活的
/// 服务端可算取值（分辨率档 / HDR），算「只去掉它还能留下多少部」。响应形状
/// 对齐前端 FilterEmptyState：`{total, suggestions: [{dim, dim_label, value, label, count}]}`。
pub(crate) async fn relax_filter(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let state_for_work = state.clone();
    let work = state
        .http_work
        .run(ORGANIZE_READ_DEADLINE, move || {
            relax_filter_inner(&state_for_work, &id, &body)
        })
        .await;
    match work {
        Ok(response) => response,
        Err(error) => blocking_error(error, "organize.timeout"),
    }
}

fn relax_filter_inner(state: &ApiState, id: &str, body: &Value) -> Response {
    let store = state.store.lock();
    let Some(library) = store.get_library(id).ok().flatten() else {
        return missing_library();
    };
    let resolutions: Vec<String> = body["resolutions"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let want_hdr: Option<bool> = body["hdr"].as_bool();
    let mut by_media: std::collections::HashMap<domain::MediaId, Vec<domain::LedgerRow>> =
        std::collections::HashMap::new();
    for row in rows_in_library(&store, &library) {
        by_media.entry(row.media_id).or_default().push(row);
    }
    let count_with = |drop_resolutions: &[String], drop_hdr: bool| -> usize {
        by_media
            .values()
            .filter(|rows| {
                let keeps_res = drop_resolutions.is_empty()
                    || rows.iter().any(|row| {
                        row.resolution
                            .as_deref()
                            .is_some_and(|r| drop_resolutions.iter().any(|w| w == r))
                    });
                if !keeps_res {
                    return false;
                }
                if !drop_hdr {
                    return true;
                }
                let has_hdr = rows.iter().any(|row| row.hdr.is_some());
                has_hdr == want_hdr.unwrap_or(false) || want_hdr.is_none()
            })
            .count()
    };
    let active = !resolutions.is_empty() || want_hdr.is_some();
    let mut suggestions = Vec::new();
    if active {
        for res in &resolutions {
            let kept: Vec<String> = resolutions
                .iter()
                .filter(|r| r.as_str() != res.as_str())
                .cloned()
                .collect();
            suggestions.push(json!({
                "dim": "resolutions",
                "dim_label": "分辨率",
                "value": res,
                "label": res,
                "count": count_with(&kept, true),
            }));
        }
        if let Some(hdr) = want_hdr {
            suggestions.push(json!({
                "dim": "hdr",
                "dim_label": "HDR",
                "value": hdr.to_string(),
                "label": if hdr { "只看 HDR" } else { "只看 SDR" },
                "count": count_with(&resolutions, false),
            }));
        }
    }
    ok(json!({
        "total": by_media.len(),
        "suggestions": suggestions,
    }))
    .into_response()
}

/// GET /libraries/{id}/items/{itemId}/similar — TMDB similar titles as
/// poster-wall hits（库条目详情页的「相似推荐」行）。
pub(crate) async fn similar(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
) -> Response {
    let state_for_work = state.clone();
    let work = state
        .http_work
        .run(ORGANIZE_READ_DEADLINE, move || {
            similar_inner(&state_for_work, &id, &item_id, user_id)
        })
        .await;
    match work {
        Ok(response) => response,
        Err(error) => blocking_error(error, "organize.timeout"),
    }
}

fn similar_inner(
    state: &ApiState,
    id: &str,
    item_id: &str,
    user_id: Option<domain::UserId>,
) -> Response {
    let store = state.store.lock();
    let library = match require_visible_library(&store, id, user_id) {
        Ok(library) => library,
        Err(response) => return response,
    };
    let media_id = match domain::MediaId::from_str(item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let Some(media) = store.get_media(media_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
    };
    let belongs_to_library = store.list_ledger().unwrap_or_default().iter().any(|row| {
        row.media_id == media_id
            && library_for_row(&store, row, &media).is_some_and(|owner| owner.id == library.id)
    });
    if !belongs_to_library {
        return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
    }
    let Some(tmdb_id) = media.tmdb_id.clone() else {
        return ok_list(Vec::new()).into_response();
    };
    drop(store);
    let hits = state
        .catalog
        .similar(media.kind, &tmdb_id)
        .unwrap_or_default();
    let items: Vec<Value> = hits
        .into_iter()
        .map(|hit| {
            json!({
                "id": hit.media.tmdb_id,
                "media_item_id": null,
                "kind": hit.media.kind.as_str(),
                "title": hit.media.title,
                "year": hit.media.year,
                "tmdb_id": hit.media.tmdb_id,
                "poster_url": hit.poster_path.as_deref().map(|p| format!("https://image.tmdb.org/t/p/w342{p}")),
            })
        })
        .collect();
    ok_list(items).into_response()
}
