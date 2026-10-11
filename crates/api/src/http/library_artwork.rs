//! Artwork: cover candidates, select, upload, and the poster byte endpoint.
//! Covers are stored as `poster.jpg` beside the owning file; a per-Media
//! `artwork.{media_id}` setting locks a manual selection so automatic
//! refreshes never overwrite it (Emby-style artwork lock).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::MediaId;
use serde_json::{Value, json};
use std::str::FromStr;

use crate::http::{err, ok};
use crate::management::ApiState;

use super::library::{library_for_row, poster_path, preferred_row, require_visible_library};

fn artwork_key(media_id: MediaId) -> String {
    format!("artwork.{}", media_id)
}

fn poster_locked(store: &crate::Store, media_id: MediaId) -> bool {
    store
        .get_setting(&artwork_key(media_id))
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| value["poster_locked"].as_bool())
        .unwrap_or(false)
}

/// GET /libraries/{id}/items/{item_id}/artwork/candidates
pub(crate) async fn artwork_candidates(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
) -> Response {
    let store = state.store.lock();
    let library = match require_visible_library(&store, &id, user_id) {
        Ok(library) => library,
        Err(response) => return response,
    };
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
        .filter(|row| {
            library_for_row(&store, row, &media).is_some_and(|owner| owner.id == library.id)
        })
        .collect();
    if rows.is_empty() {
        return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
    }
    let selected = preferred_row(&rows);
    let current = selected
        .and_then(poster_path)
        .map(|p| p.display().to_string());
    let locked = poster_locked(&store, media_id);
    drop(store);

    let mut posters: Vec<Value> = Vec::new();
    if let (Some(path), Some(row)) = (&current, selected) {
        posters.push(json!({
            "file_path": path,
            "preview_url": crate::http::library::artwork_url("posters", row.id),
            "width": 342,
            "height": 513,
            "language": null,
            "vote_average": null,
            "vote_count": null,
        }));
    }
    // Remote metadata candidate, when a configured provider can reach it.
    if let Some(tmdb_id) = media.tmdb_id.clone() {
        if let Ok(Some(url)) = state.catalog.poster_url(media.kind, &tmdb_id) {
            posters.push(json!({
                "file_path": url,
                "preview_url": url,
                "width": null,
                "height": null,
                "language": null,
                "vote_average": null,
                "vote_count": null,
            }));
        }
    }
    ok(json!({
        "posters": posters,
        "backdrops": [],
        "current_poster": current,
        "current_backdrop": null,
        "poster_locked": locked,
        "backdrop_locked": false,
    }))
    .into_response()
}

fn artwork_target(
    store: &crate::Store,
    media_id: MediaId,
    library_id: Option<&str>,
    is_backdrop: bool,
) -> Result<std::path::PathBuf, Response> {
    let all_rows: Vec<domain::LedgerRow> = store
        .list_ledger()
        .unwrap_or_default()
        .into_iter()
        .filter(|row| row.media_id == media_id)
        .collect();
    let media = store.get_media(media_id).ok().flatten();
    let rows: Vec<domain::LedgerRow> = if let (Some(lib_id), Some(m)) = (library_id, &media) {
        all_rows
            .into_iter()
            .filter(|row| {
                store
                    .library_for_path(std::path::Path::new(&row.path), m.kind)
                    .ok()
                    .flatten()
                    .is_some_and(|l| l.id == lib_id)
            })
            .collect()
    } else {
        all_rows
    };
    let Some(target_row) = preferred_row(&rows) else {
        return Err(err(
            StatusCode::NOT_FOUND,
            "library.item_missing",
            "条目没有可用文件",
        ));
    };
    let file_name = if is_backdrop {
        "fanart.jpg"
    } else {
        "poster.jpg"
    };
    std::path::Path::new(&target_row.path)
        .parent()
        .map(|dir| dir.join(file_name))
        .ok_or_else(|| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "artwork.path",
                "无法定位封面目录",
            )
        })
}

async fn jpeg_response(
    path: std::path::PathBuf,
    code: &'static str,
    message: &'static str,
) -> Response {
    match tokio::task::spawn_blocking(move || std::fs::read(path)).await {
        Ok(Ok(bytes)) => (
            [(axum::http::header::CONTENT_TYPE, image_content_type(&bytes))],
            bytes,
        )
            .into_response(),
        _ => err(StatusCode::NOT_FOUND, code, message),
    }
}

pub(crate) fn image_content_type(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else {
        // Existing artwork paths have a `.jpg` contract. The explicit PNG
        // and WebP signatures above override it for uploaded alternatives.
        "image/jpeg"
    }
}

fn visible_ledger_row(
    store: &crate::Store,
    compact_id: &str,
    user_id: Option<domain::UserId>,
) -> Option<domain::LedgerRow> {
    let row = store.get_ledger(compact_id).ok().flatten()?;
    let media = store.get_media(row.media_id).ok().flatten()?;
    super::library::row_visible_to_user(store, &row, &media, user_id).then_some(row)
}

/// POST /libraries/{id}/items/{item_id}/artwork/select — apply a candidate
/// URL/path, or restore automatic selection (file_path=null).
pub(crate) async fn select_artwork(
    State(state): State<ApiState>,
    Path((id, item_id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    let media_id = match MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let lease = match state.acquire_library_work(&id) {
        Ok(lease) => lease,
        Err(_) => return err(StatusCode::CONFLICT, "library.deleting", "Library 正在删除"),
    };
    let is_backdrop = body.get("kind").and_then(Value::as_str) == Some("backdrop");
    let file_path = body["file_path"].as_str().map(str::to_string);
    let store = state.store.lock();
    let Some(media) = store.get_media(media_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在");
    };
    let target = match artwork_target(&store, media_id, Some(&id), is_backdrop) {
        Ok(target) => target,
        Err(response) => return response,
    };
    let preferred_path = preferred_row(
        &store
            .list_ledger()
            .unwrap_or_default()
            .into_iter()
            .filter(|row| row.media_id == media_id)
            .collect::<Vec<_>>(),
    )
    .map(|row| std::path::PathBuf::from(&row.path));

    match file_path {
        None => {
            let _ = store.delete_setting(&artwork_key(media_id));
            drop(store);
            if let Some(preferred_path) = preferred_path {
                if is_backdrop {
                    let _ = crate::poster_fetch::attach_backdrop(&state, &media, &preferred_path);
                } else {
                    let _ = crate::poster_fetch::attach_poster(&state, &media, &preferred_path);
                }
            }
            ok(json!({ "locked": false, "automatic": true })).into_response()
        }
        Some(source) => {
            drop(store);
            let bytes = if source.starts_with("http://") || source.starts_with("https://") {
                match state.poster_fetch.get(&source) {
                    Ok(bytes) if !bytes.is_empty() => bytes,
                    _ => return err(StatusCode::BAD_GATEWAY, "artwork.fetch", "下载候选图片失败"),
                }
            } else {
                match std::fs::read(&source) {
                    Ok(bytes) if !bytes.is_empty() => bytes,
                    _ => {
                        return err(
                            StatusCode::BAD_REQUEST,
                            "artwork.file",
                            "候选图片文件不存在",
                        );
                    }
                }
            };
            if let Err(error) = lease.publish(|| std::fs::write(&target, &bytes).map_err(|e| e.to_string())) {
                return err(
                    StatusCode::CONFLICT,
                    "library.deleting",
                    &error,
                );
            }
            let store = state.store.lock();
            let _ = store.put_setting(
                &artwork_key(media_id),
                &json!({ "poster_locked": true, "source": source }).to_string(),
            );
            ok(json!({ "locked": true, "automatic": false })).into_response()
        }
    }
}

/// POST /libraries/{id}/items/{item_id}/artwork/upload — custom JPEG/PNG/WebP
/// as a base64 data URL.
pub(crate) async fn upload_artwork(
    State(state): State<ApiState>,
    Path((id, item_id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    use base64::Engine;
    let media_id = match MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let lease = match state.acquire_library_work(&id) {
        Ok(lease) => lease,
        Err(_) => return err(StatusCode::CONFLICT, "library.deleting", "Library 正在删除"),
    };
    let is_backdrop = body.get("kind").and_then(Value::as_str) == Some("backdrop");
    let data_url = body["data_url"].as_str().unwrap_or_default();
    let (_, encoded) = match data_url.split_once(',') {
        Some(parts) => parts,
        None => return err(StatusCode::BAD_REQUEST, "artwork.upload", "图片格式无效"),
    };
    let bytes = match base64::engine::general_purpose::STANDARD.decode(encoded) {
        Ok(bytes) if !bytes.is_empty() && bytes.len() <= 12 * 1024 * 1024 => bytes,
        Ok(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "artwork.upload",
                "图片为空或超过 12MB",
            );
        }
        Err(_) => return err(StatusCode::BAD_REQUEST, "artwork.upload", "图片编码无效"),
    };
    let target = {
        let store = state.store.lock();
        match artwork_target(&store, media_id, Some(&id), is_backdrop) {
            Ok(target) => target,
            Err(response) => return response,
        }
    };
    let write_target = target.clone();
    let write_bytes = bytes.clone();
    let write =
        tokio::task::spawn_blocking(move || {
            lease.publish(|| std::fs::write(write_target, write_bytes).map_err(|e| e.to_string()))
        }).await;
    if let Err(error) = write.unwrap_or_else(|error| Err(error.to_string()))
    {
        return err(
            StatusCode::CONFLICT,
            "library.deleting",
            &error,
        );
    }
    let _ = state.store.lock().put_setting(
        &artwork_key(media_id),
        &json!({ "poster_locked": true, "source": "upload" }).to_string(),
    );
    ok(json!({ "locked": true, "bytes": bytes.len() })).into_response()
}

fn extract_user_id(
    store: &parking_lot::Mutex<crate::Store>,
    user_ext: Option<axum::Extension<Option<domain::UserId>>>,
    headers: &axum::http::HeaderMap,
) -> Option<domain::UserId> {
    if let Some(user_id) = user_ext.and_then(|u| u.0) {
        return Some(user_id);
    }
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if let Some(token) = bearer {
        return store.lock().user_id_by_token(token).ok().flatten();
    }
    None
}

/// GET /fanart/{ledger_id} — the fanart.jpg bytes beside a ledger file.
pub(crate) async fn fanart(
    State(state): State<ApiState>,
    headers: axum::http::HeaderMap,
    user_id: Option<axum::Extension<Option<domain::UserId>>>,
    Path(ledger_id): Path<String>,
) -> Response {
    let user_id = extract_user_id(&state.store, user_id, &headers);
    let compact = ledger_id.replace('-', "");
    let row = {
        let store = state.store.lock();
        match visible_ledger_row(&store, &compact, user_id) {
            Some(row) => row,
            _ => return err(StatusCode::NOT_FOUND, "poster.missing", "背景图不存在"),
        }
    };
    let file_dir = std::path::Path::new(&row.path).parent();
    let fanart = file_dir.and_then(|dir| {
        // 兼容平铺（文件旁）与整理后（<show>/Season N → show 根）布局。
        let here = dir.join("fanart.jpg");
        let up = dir.parent().map(|p| p.join("fanart.jpg"));
        [Some(here), up].into_iter().flatten().find(|p| p.is_file())
    });
    let fanart = match fanart {
        Some(f) => Some(f),
        None => {
            // 本地尚无背景图时，尝试即时从 TMDB 拉取并落地
            let media = state.store.lock().get_media(row.media_id).ok().flatten();
            if let Some(m) = media {
                crate::poster_fetch::attach_backdrop(&state, &m, std::path::Path::new(&row.path));
            }
            file_dir.and_then(|dir| {
                let here = dir.join("fanart.jpg");
                let up = dir.parent().map(|p| p.join("fanart.jpg"));
                [Some(here), up].into_iter().flatten().find(|p| p.is_file())
            })
        }
    };
    let Some(fanart) = fanart else {
        return err(StatusCode::NOT_FOUND, "poster.missing", "背景图不存在");
    };
    jpeg_response(fanart, "poster.missing", "背景图不存在").await
}

/// GET /stills/{ledger_id} — the episode-specific still beside a ledger file.
pub(crate) async fn stills(
    State(state): State<ApiState>,
    headers: axum::http::HeaderMap,
    user_id: Option<axum::Extension<Option<domain::UserId>>>,
    Path(ledger_id): Path<String>,
) -> Response {
    let user_id = extract_user_id(&state.store, user_id, &headers);
    let compact = ledger_id.replace('-', "");
    let row = {
        let store = state.store.lock();
        match visible_ledger_row(&store, &compact, user_id) {
            Some(row) => row,
            _ => return err(StatusCode::NOT_FOUND, "poster.missing", "剧照不存在"),
        }
    };
    let still = crate::episode_still::existing(std::path::Path::new(&row.path));
    let still = match still {
        Some(s) => Some(s),
        None => {
            // 本地尚无分集剧照时，回退尝试使用同条目的 fanart 作为剧照输出
            let file_dir = std::path::Path::new(&row.path).parent();
            file_dir.and_then(|dir| {
                let here = dir.join("fanart.jpg");
                let up = dir.parent().map(|p| p.join("fanart.jpg"));
                [Some(here), up].into_iter().flatten().find(|p| p.is_file())
            })
        }
    };
    let Some(still) = still else {
        return err(StatusCode::NOT_FOUND, "poster.missing", "剧照不存在");
    };
    jpeg_response(still, "poster.missing", "剧照不存在").await
}

/// GET /posters/{ledger_id} — the poster.jpg bytes beside a ledger file.
pub(crate) async fn poster(
    State(state): State<ApiState>,
    headers: axum::http::HeaderMap,
    user_id: Option<axum::Extension<Option<domain::UserId>>>,
    Path(ledger_id): Path<String>,
) -> Response {
    let user_id = extract_user_id(&state.store, user_id, &headers);
    let compact = ledger_id.replace('-', "");
    let row = {
        let store = state.store.lock();
        match visible_ledger_row(&store, &compact, user_id) {
            Some(row) => row,
            _ => return err(StatusCode::NOT_FOUND, "poster.missing", "海报不存在"),
        }
    };
    let poster = std::path::Path::new(&row.path).parent().and_then(|dir| {
        let here = dir.join("poster.jpg");
        let up = dir.parent().map(|p| p.join("poster.jpg"));
        [Some(here), up].into_iter().flatten().find(|p| p.is_file())
    });
    let Some(poster) = poster else {
        return err(StatusCode::NOT_FOUND, "poster.missing", "海报不存在");
    };
    jpeg_response(poster, "poster.missing", "海报不存在").await
}

/// GET /libraries/{id}/cover — visible library cover.
pub(crate) async fn library_cover(
    State(state): State<ApiState>,
    headers: axum::http::HeaderMap,
    user_id: Option<axum::Extension<Option<domain::UserId>>>,
    Path(id): Path<String>,
) -> Response {
    let user_id = extract_user_id(&state.store, user_id, &headers);
    let (store_lock, cover_path, kind) = {
        let store = state.store.lock();
        let Some(library) = store.get_library(&id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "library.missing", "媒体库不存在");
        };
        if !crate::http::library::library_visible(&store, &library, user_id) {
            return err(StatusCode::NOT_FOUND, "library.missing", "媒体库不存在");
        }
        let cover = crate::http::library::library_cover_path(&store, &library);
        let kind = library.kind;
        (drop(store), cover, kind)
    };
    let _ = store_lock;
    let Some(cover) = cover_path else {
        return match crate::media_server_provider::default_library_cover(kind) {
            Ok(bytes) => (
                [(axum::http::header::CONTENT_TYPE, image_content_type(&bytes))],
                bytes,
            )
                .into_response(),
            Err(error) => {
                tracing::error!(%error, library_id = %id, "failed to generate default library cover");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        };
    };
    jpeg_response(cover, "cover.missing", "封面读取失败").await
}

/// POST /libraries/{id}/cover — 用户主动上传/更换媒体库封面（JPEG/PNG/WebP base64 data URL）
pub(crate) async fn upload_library_cover(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    use base64::Engine;
    let (covers_dir, library_id) = {
        let store = state.store.lock();
        let Some(library) = store.get_library(&id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "library.missing", "媒体库不存在");
        };
        if !crate::http::library::library_visible(&store, &library, user_id) {
            return err(StatusCode::NOT_FOUND, "library.missing", "媒体库不存在");
        }
        (store.library_covers_dir(), library.id)
    };

    let data_url = body["data_url"].as_str().unwrap_or_default();
    let (_, encoded) = match data_url.split_once(',') {
        Some(parts) => parts,
        None => return err(StatusCode::BAD_REQUEST, "cover.upload", "图片格式无效"),
    };
    let bytes = match base64::engine::general_purpose::STANDARD.decode(encoded) {
        Ok(bytes) if !bytes.is_empty() && bytes.len() <= 12 * 1024 * 1024 => bytes,
        Ok(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "cover.upload",
                "图片为空或超过 12MB",
            );
        }
        Err(_) => return err(StatusCode::BAD_REQUEST, "cover.upload", "图片编码无效"),
    };

    let _ = std::fs::create_dir_all(&covers_dir);
    let cover_target = covers_dir.join(format!("library-{}.jpg", library_id));
    let write_target = cover_target.clone();
    let write_bytes = bytes.clone();
    let write =
        tokio::task::spawn_blocking(move || std::fs::write(write_target, write_bytes)).await;
    if let Err(error) = write.unwrap_or_else(|error| Err(std::io::Error::other(error.to_string())))
    {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "cover.write",
            &error.to_string(),
        );
    }

    {
        let store = state.store.lock();
        if let Err(error) =
            store.set_library_cover_path(&library_id, Some(&cover_target.display().to_string()))
        {
            tracing::warn!(%error, library_id, "failed to persist uploaded library cover path");
        }
        crate::http::library::mark_library_cover_manual(&store, &library_id);
    }
    ok(json!({ "bytes": bytes.len(), "path": cover_target.display().to_string() })).into_response()
}

/// DELETE /libraries/{id}/cover — 删除/清空媒体库自定义封面
pub(crate) async fn delete_library_cover(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path(id): Path<String>,
) -> Response {
    let (cover_path_opt, covers_dir, library_id) = {
        let store = state.store.lock();
        let Some(library) = store.get_library(&id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "library.missing", "媒体库不存在");
        };
        if !crate::http::library::library_visible(&store, &library, user_id) {
            return err(StatusCode::NOT_FOUND, "library.missing", "媒体库不存在");
        }
        (
            library.cover_path.clone(),
            store.library_covers_dir(),
            library.id,
        )
    };

    // 删除物理文件
    if let Some(cp) = cover_path_opt {
        let _ = std::fs::remove_file(cp);
    }
    let default_file = covers_dir.join(format!("library-{}.jpg", library_id));
    let _ = std::fs::remove_file(default_file);

    // 设置 cover_path 为特殊标记 "none"，避免再次被自动填充唤醒
    let _ = state
        .store
        .lock()
        .set_library_cover_path(&library_id, Some("none"));
    ok(json!({ "deleted": true })).into_response()
}

/// POST /libraries/{id}/cover/generate — 自动/手动重新生成媒体库艺术封面
pub(crate) async fn generate_library_cover(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    use base64::Engine;
    use cover_generator::{BackgroundOption, CoverOptions, CoverStyle, generate_cover};
    use std::io::Cursor;

    let (covers_dir, library, sample_posters) = {
        let store = state.store.lock();
        let Some(library) = store.get_library(&id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "library.missing", "媒体库不存在");
        };
        if !crate::http::library::library_visible(&store, &library, user_id) {
            return err(StatusCode::NOT_FOUND, "library.missing", "媒体库不存在");
        }

        // 收集该库下多部不同作品的海报（按条目去重，避免同一部剧的多个分集重复提取同一张海报）
        let rows = store.list_ledger().unwrap_or_default();
        let mut posters = Vec::new();
        let mut seen_media_ids = std::collections::HashSet::new();
        for row in rows.iter() {
            if seen_media_ids.contains(&row.media_id) {
                continue;
            }
            let media = store.get_media(row.media_id).ok().flatten();
            let is_in_lib = media
                .as_ref()
                .and_then(|m| crate::http::library::library_for_row(&store, row, m))
                .is_some_and(|l| l.id == library.id);

            if is_in_lib {
                if let Some(poster_file) = crate::http::library::poster_path(row) {
                    if let Ok(bytes) = std::fs::read(&poster_file) {
                        if let Ok(dyn_img) = image::load_from_memory(&bytes) {
                            posters.push(dyn_img);
                            seen_media_ids.insert(row.media_id);
                            if posters.len() >= 6 {
                                break;
                            }
                        }
                    }
                }
            }
        }
        (store.library_covers_dir(), library, posters)
    };

    if sample_posters.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "cover.empty",
            "媒体库内无可用海报图片",
        );
    }

    let title_zh = body["title_zh"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| library.name.clone());
    let title_en = body["title_en"].as_str().map(|s| s.to_string());
    let preview_only = body["preview_only"].as_bool().unwrap_or(false);

    // 解析背景与风格
    let style = match body["style"].as_str() {
        Some("multi_poster_pile") | Some("multi") => CoverStyle::MultiPosterPile,
        _ => CoverStyle::MacaronCardSingle,
    };

    let background: BackgroundOption =
        serde_json::from_value(body["background"].clone()).unwrap_or_default();

    let options = CoverOptions {
        title_zh,
        title_en,
        width: 1920,
        height: 1080,
        style,
        background,
    };

    // 优先使用内置的 MoviePilot 超黑精选字体 (chaohei.ttf)，兜底本地系统粗体
    const EMBEDDED_CHAOHEI: &[u8] =
        include_bytes!("../../../cover-generator/assets/fonts/chaohei.ttf");
    let font_bytes: Vec<u8> = if !EMBEDDED_CHAOHEI.is_empty() {
        EMBEDDED_CHAOHEI.to_vec()
    } else {
        std::fs::read("/System/Library/Fonts/STHeiti Medium.ttc")
            .or_else(|_| std::fs::read("/System/Library/Fonts/Hiragino Sans GB.ttc"))
            .or_else(|_| std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"))
            .unwrap_or_default()
    };

    let library_id = library.id.clone();
    let (res, write_target, bytes) = tokio::task::spawn_blocking(move || {
        let gen_result = generate_cover(&sample_posters, &options, &font_bytes, None);
        let mut jpeg = Vec::new();
        match gen_result {
            Ok(img) => {
                if let Err(e) = img.write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
                {
                    (Err(format!("cover.encode: {e}")), None, Vec::new())
                } else {
                    let target = if !preview_only {
                        let _ = std::fs::create_dir_all(&covers_dir);
                        let file = covers_dir.join(format!("library-{}.jpg", library_id));
                        let _ = std::fs::write(&file, &jpeg);
                        Some(file)
                    } else {
                        None
                    };
                    (Ok(()), target, jpeg)
                }
            }
            Err(e) => (Err(format!("cover.generate: {e}")), None, Vec::new()),
        }
    })
    .await
    .unwrap_or_else(|e| (Err(e.to_string()), None, Vec::new()));

    if let Err(err_msg) = res {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "cover.error", &err_msg);
    }

    let data_url = format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    );

    if preview_only {
        return ok(json!({ "data_url": data_url, "preview": true })).into_response();
    }

    if let Some(target) = write_target {
        let store = state.store.lock();
        if let Err(error) =
            store.set_library_cover_path(&library.id, Some(&target.display().to_string()))
        {
            tracing::warn!(%error, library_id = %library.id, "failed to persist generated library cover path");
        }
        crate::http::library::mark_library_cover_manual(&store, &library.id);
    }

    ok(json!({
        "success": true,
        "cover_url": format!("/libraries/{}/cover", library.id),
        "data_url": data_url,
    }))
    .into_response()
}
