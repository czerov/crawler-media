//! 重新识别：把条目的身份（TMDB 锚点/标题）改挂到正确作品，或把文件拆出
//! 为「非独立作品」（extras）。前端契约对齐 ReidentifyDialog / ClaimPanels。

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{MediaId, MediaKind};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path as FsPath;
use std::str::FromStr;

use crate::blocking_budget::BlockingError;
use crate::http::library::{poster_path, preferred_row};
use crate::http::title_ref::resolve_title;
use crate::http::{err, ok};
use crate::management::ApiState;

/// Blocking-budget failure → HTTP response（上游/存储错误走各自原有路径）。
fn blocking_error(error: BlockingError) -> Response {
    match error {
        BlockingError::Busy => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请重试",
        ),
        error => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "runtime.worker",
            &error.to_string(),
        ),
    }
}

fn review_item(media: &domain::Media, poster_url: Option<String>) -> Value {
    json!({
        "media_item_id": media.id.to_string(),
        "tmdb_id": media.tmdb_id,
        "title": media.title,
        "year": media.year,
        "poster_url": poster_url,
    })
}

/// POST /libraries/{id}/items/{item_id}/reidentification-preview —
/// 当前身份 + 文件名解析出的搜索种子（前端据此给候选）。
pub(crate) async fn preview(
    State(state): State<ApiState>,
    Path((_id, item_id)): Path<(String, String)>,
) -> Response {
    let store = state.store.lock();
    let media_id = match MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let Some(media) = store.get_media(media_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
    };
    let rows: Vec<domain::LedgerRow> = store
        .list_ledger()
        .unwrap_or_default()
        .into_iter()
        .filter(|row| row.media_id == media_id)
        .collect();
    let search_seed = rows
        .iter()
        .filter_map(|row| {
            FsPath::new(&row.path)
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| release::parse(name).title)
        })
        .next()
        .unwrap_or_else(|| media.title.clone());
    let selected = preferred_row(&rows);
    let poster = selected.and_then(poster_path);
    let poster_url = selected
        .filter(|_| poster.is_some())
        .map(|row| crate::http::library::artwork_url("posters", row.id));
    let groups: Vec<Value> = if rows.is_empty() {
        Vec::new()
    } else {
        vec![json!({
            "key": media.id.to_string(),
            "outcome": {
                "media_item_id": media.id.to_string(),
                "tmdb_id": media.tmdb_id,
                "title": media.title,
                "year": media.year,
                "poster_url": poster_url.clone(),
                "source": "current",
                "same_as_current": true,
                "reason": null,
                "code": null,
                "candidates": [],
            },
            "file_ids": rows.iter().map(|row| row.id.to_string()).collect::<Vec<_>>(),
            "file_count": rows.len(),
            "total_size_bytes": 0,
            "sample_names": rows.iter().take(3).filter_map(|row| {
                FsPath::new(&row.path).file_name().and_then(|n| n.to_str()).map(str::to_string)
            }).collect::<Vec<_>>(),
        })]
    };
    ok(json!({
        "current": review_item(&media, poster_url),
        "movie": media.kind == MediaKind::Movie,
        "groups": groups,
        "skipped_missing": 0,
        "pinned_identity": false,
        "unreachable": false,
        "search_seed": search_seed,
    }))
    .into_response()
}

/// POST /libraries/{id}/items/{item_id}/reidentifications —
/// 把条目身份改挂到 `title_ref` 指向的作品并重刷元数据。
pub(crate) async fn reidentify(State(state): State<ApiState>, Json(body): Json<Value>) -> Response {
    let state_for_work = state.clone();
    let work = state
        .http_work
        .run_to_completion(move || reidentify_inner(&state_for_work, &body))
        .await;
    match work {
        Ok(response) => response,
        Err(error) => blocking_error(error),
    }
}

/// 整个同步重识别用例（解析 → lease → 改身份 → 抓海报/NFO）都在 blocking
/// 闭包内，LibraryLease 持有到真实工作结束。
fn reidentify_inner(state: &ApiState, body: &Value) -> Response {
    let store = state.store.lock();
    let Some(title_ref) = body["title_ref"].as_str() else {
        return err(
            StatusCode::BAD_REQUEST,
            "reidentify.invalid",
            "缺少 title_ref",
        );
    };
    let Some(file_id) = body["file_ids"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
    else {
        return err(
            StatusCode::BAD_REQUEST,
            "reidentify.invalid",
            "缺少 file_ids",
        );
    };
    let Some(row) = store.get_ledger(&file_id.replace('-', "")).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "library.file_missing", "文件不存在");
    };
    let media_id = row.media_id;
    let Some(media) = store.get_media(media_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
    };
    let rows: Vec<domain::LedgerRow> = store
        .list_ledger()
        .unwrap_or_default()
        .into_iter()
        .filter(|row| row.media_id == media_id)
        .collect();
    let claimed = rows.len();
    let owners: HashSet<String> = rows
        .iter()
        .filter_map(|row| {
            crate::http::library::library_for_row(&store, row, &media).map(|owner| owner.id)
        })
        .collect();
    drop(store);
    let leases = match owners
        .iter()
        .map(|id| state.acquire_library_work(id))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(leases) => leases,
        Err(error) => return err(StatusCode::CONFLICT, "library.frozen", &error),
    };
    let cancelled = || leases.iter().any(|lease| lease.control().is_cancelled());
    let target = resolve_title(state, title_ref);
    match target {
        Ok(Some(target)) => {
            // 更新 media 身份。
            {
                let store = state.store.lock();
                if cancelled()
                    || owners
                        .iter()
                        .any(|id| !store.library_accepts_work(id).unwrap_or(false))
                {
                    return err(
                        StatusCode::CONFLICT,
                        "library.frozen",
                        "Library work cancelled",
                    );
                }
                let mut updated = media.clone();
                updated.title = target.title.clone();
                updated.year = target.year;
                updated.tmdb_id = target.tmdb_id.clone();
                updated.original_title = target.original_title.clone();
                let _ = store.update_media(&updated);
            }
            // 重刷元数据（海报/背景/NFO）。
            if let Some(row) = rows.first() {
                let path = FsPath::new(&row.path).to_path_buf();
                if cancelled() {
                    return err(
                        StatusCode::CONFLICT,
                        "library.frozen",
                        "Library work cancelled",
                    );
                }
                let _ = crate::poster_fetch::attach_poster(state, &target, &path);
                let _ = crate::poster_fetch::attach_backdrop(state, &target, &path);
                if let Some(tmdb_id) = target.tmdb_id.clone() {
                    match crate::scrape_metadata::fetch_tmdb_metadata(state, target.kind, &tmdb_id)
                    {
                        Ok(Some(meta)) => {
                            if cancelled() {
                                return err(
                                    StatusCode::CONFLICT,
                                    "library.frozen",
                                    "Library work cancelled",
                                );
                            }
                            let nfo = crate::scrape_metadata::nfo_from_tmdb(&target, &meta);
                            if target.kind == MediaKind::Tv {
                                crate::scrape_metadata::scrape_tv_sidecars(
                                    state,
                                    &target,
                                    &crate::scrape_metadata::show_root(&path, row),
                                    &rows,
                                );
                            } else if let Some(stem) =
                                path.file_stem().and_then(|value| value.to_str())
                            {
                                let nfo_path = path.with_file_name(format!("{stem}.nfo"));
                                if let Some(lease) = leases.iter().find(|lease| {
                                    state
                                        .store
                                        .lock()
                                        .library_for_path_strict(&path, media.kind)
                                        .ok()
                                        .flatten()
                                        .is_some_and(|owner| owner.id == lease.library_id())
                                }) {
                                    let _ = lease.control().signal().publish(|| {
                                        crate::scrape_metadata::write_nfo(&nfo_path, &target, &nfo);
                                        Ok(())
                                    });
                                }
                                let movie_root = path.parent().unwrap_or(&path);
                                crate::scrape_metadata::scrape_movie_fanart(state, &target, movie_root);
                            }
                        }
                        Ok(None) => {
                            tracing::debug!(%tmdb_id, "catalog returned no metadata during reidentification")
                        }
                        Err(error) => {
                            tracing::warn!(%error, %tmdb_id, "failed to scrape metadata during reidentification")
                        }
                    }
                }
            }
            ok(json!({ "claimed": claimed })).into_response()
        }
        Ok(None) => err(
            StatusCode::NOT_FOUND,
            "reidentify.no_match",
            "没有匹配到作品",
        ),
        Err(error) => err(StatusCode::BAD_GATEWAY, "reidentify.upstream", &error),
    }
}

/// POST /libraries/{id}/items/{item_id}/reidentifications/extras —
/// 把条目文件标为「非独立作品」：从台账拆出进未识别清单。
pub(crate) async fn extras(State(state): State<ApiState>, Json(body): Json<Value>) -> Response {
    let store = state.store.lock();
    let target_ids: HashSet<String> = body["file_ids"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.replace('-', ""))
                .collect()
        })
        .unwrap_or_default();
    if target_ids.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "reidentify.invalid",
            "缺少 file_ids",
        );
    }
    let mut detached = 0usize;
    for file_id in target_ids {
        if let Some(row) = store.get_ledger(&file_id).ok().flatten() {
            // 拆出台账 → 未识别清单（文件保留在磁盘，重新扫描可再认领）。
            if store
                .insert_unidentified(row.path.clone(), row.confidence)
                .is_ok()
                && store.delete_ledger_path(&row.path).is_ok()
            {
                detached += 1;
            }
        }
    }
    ok(json!({ "detached": detached })).into_response()
}
