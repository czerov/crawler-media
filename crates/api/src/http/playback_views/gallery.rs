use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

use crate::http::ok_list;
use crate::management::ApiState;

/// GET /playback/favorites/gallery — 收藏图廊：每部作品的海报/背景/剧照，
/// 形状对齐前端 LibraryGalleryGroup（listFavoritesGallery 直读）。
pub(crate) async fn favorites_gallery(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let limit = query
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20)
        .clamp(1, 200);
    let offset = query
        .get("offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let sort_key = query
        .get("sort")
        .map(String::as_str)
        .unwrap_or("updated_at");
    let is_desc = query
        .get("order")
        .map(|o| o == "desc")
        .unwrap_or_else(|| sort_key != "title");

    // Phase 1: In-lock DB reads only (media, ledger rows, favorites)
    struct MediaGalleryCandidate {
        media: domain::Media,
        library_id: String,
        preferred_row: Option<domain::LedgerRow>,
        owned_rows: Vec<domain::LedgerRow>,
    }

    let candidates: Vec<MediaGalleryCandidate> = {
        let store = state.store.lock();
        let all_rows = store.list_ledger().unwrap_or_default();
        let media_ids: HashSet<domain::MediaId> = all_rows
            .iter()
            .filter_map(|row| {
                let media = store.get_media(row.media_id).ok().flatten()?;
                crate::http::library::row_visible_to_user(&store, row, &media, Some(user_id))
                    .then_some(row.media_id)
            })
            .collect();
        let mut by_media: HashMap<domain::MediaId, i64> = HashMap::new();
        for media_id in media_ids {
            for row in store.unit_rows(user_id, media_id).unwrap_or_default() {
                if row.favorite {
                    by_media.insert(media_id, row.updated_at);
                }
            }
        }

        let mut list: Vec<(domain::MediaId, i64)> = by_media.into_iter().collect();
        if sort_key == "title" {
            list.sort_by(|(a_id, _), (b_id, _)| {
                let a_title = store
                    .get_media(*a_id)
                    .ok()
                    .flatten()
                    .map(|m| m.title)
                    .unwrap_or_default();
                let b_title = store
                    .get_media(*b_id)
                    .ok()
                    .flatten()
                    .map(|m| m.title)
                    .unwrap_or_default();
                if is_desc {
                    b_title.cmp(&a_title)
                } else {
                    a_title.cmp(&b_title)
                }
            });
        } else {
            list.sort_by(|(_, a_up), (_, b_up)| {
                if is_desc {
                    b_up.cmp(a_up)
                } else {
                    a_up.cmp(b_up)
                }
            });
        }

        list.into_iter()
            .skip(offset)
            .take(limit)
            .filter_map(|(media_id, _)| {
                let media = store.get_media(media_id).ok().flatten()?;
                let owned: Vec<domain::LedgerRow> = all_rows
                    .iter()
                    .filter(|r| r.media_id == media_id)
                    .filter(|r| {
                        crate::http::library::row_visible_to_user(&store, r, &media, Some(user_id))
                    })
                    .cloned()
                    .collect();
                if owned.is_empty() {
                    return None;
                }
                let refs: Vec<&domain::LedgerRow> = owned.iter().collect();
                let library_id = refs
                    .first()
                    .and_then(|row| crate::http::library::library_for_row(&store, row, &media))
                    .map(|library| library.id)
                    .unwrap_or_default();
                let pref = crate::http::library::preferred_row(&refs).cloned();
                Some(MediaGalleryCandidate {
                    media,
                    library_id,
                    preferred_row: pref,
                    owned_rows: owned,
                })
            })
            .collect()
    };

    // Phase 2: Out-of-lock filesystem observation under http_work budget
    let files = state.browse_files.clone();
    let groups_res = state
        .http_work
        .run(std::time::Duration::from_secs(30), move || {
            let mut groups = Vec::new();
            for cand in candidates {
                let mut images = Vec::new();
                if let Some(ref row) = cand.preferred_row {
                    let dir = std::path::Path::new(&row.path)
                        .parent()
                        .map(|p| p.to_path_buf());
                    if let Some(dir) = dir {
                        if files.is_file(&dir.join("poster.jpg")) {
                            images.push(json!({
                                "kind": "poster",
                                "url": crate::http::library::artwork_url("posters", row.id),
                                "aspect": 0.667,
                                "label": "海报",
                                "season": null,
                                "episode": null,
                                "t_seconds": null,
                            }));
                        }
                        if files.is_file(&dir.join("fanart.jpg")) {
                            images.push(json!({
                                "kind": "backdrop",
                                "url": crate::http::library::artwork_url("fanart", row.id),
                                "aspect": 1.778,
                                "label": "背景",
                                "season": null,
                                "episode": null,
                                "t_seconds": null,
                            }));
                        }
                        for row in &cand.owned_rows {
                            if row.episode.is_some()
                                && crate::episode_still::existing_with(
                                    std::path::Path::new(&row.path),
                                    files.as_ref(),
                                )
                                .is_some()
                            {
                                images.push(json!({
                                    "kind": "still",
                                    "url": crate::http::library::artwork_url("stills", row.id),
                                    "aspect": 1.778,
                                    "label": "剧照",
                                    "season": row.season,
                                    "episode": row.episode,
                                    "t_seconds": null,
                                }));
                            }
                        }
                    }
                }
                groups.push(json!({
                    "media_item_id": cand.media.id.to_string(),
                    "library_id": cand.library_id,
                    "kind": cand.media.kind.as_str(),
                    "title": cand.media.title,
                    "year": cand.media.year,
                    "is_favorite": true,
                    "images": images,
                }));
            }
            groups
        })
        .await;

    let groups = match groups_res {
        Ok(groups) => groups,
        Err(crate::blocking_budget::BlockingError::Busy) => {
            return crate::http::err(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "runtime.busy",
                "服务繁忙，请重试",
            );
        }
        Err(crate::blocking_budget::BlockingError::Timeout) => {
            return crate::http::err(
                axum::http::StatusCode::GATEWAY_TIMEOUT,
                "gallery.timeout",
                "收藏图廊加载超时",
            );
        }
        Err(crate::blocking_budget::BlockingError::Worker(msg)) => {
            return crate::http::err(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "gallery.worker",
                &msg,
            );
        }
    };
    ok_list(groups).into_response()
}
