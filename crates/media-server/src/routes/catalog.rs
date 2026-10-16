//! Jellyfin/Emby catalog: items, search hints, genres and studios.

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::AuthUser;
use crate::dto::item_dto_json;

use super::item_filters::{self, filter_items};
use super::parent_scope::{
    ParentScope, classify_genre_or_studio, library_parent, require_user_path, resolve_parent_scope,
};
use super::{AppState, ItemsQuery, UserItemPath, UserPath, item_id_for_user};

pub(super) async fn items(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserPath>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    require_user_path(state.provider.as_ref(), user.id, path.user_id.as_deref()).await?;
    let libraries = state
        .provider
        .list_visible_libraries(user.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let parent = query.parent_id.as_deref();
    let series = query.series_id.as_deref();
    let scope =
        resolve_parent_scope(state.provider.as_ref(), user.id, parent, series, &libraries).await?;
    // Genre / Studio 只能靠 item metadata 判断（既有行为是在拿到 items 之后判定），
    // 所以这里先按廉价分类取 items，稍后再升级 scope。
    let needs_items = matches!(scope, ParentScope::Unknown);
    let parent_id_for_provider = match &scope {
        ParentScope::Library(id) | ParentScope::Series(id) => Some(id.as_str()),
        _ => None,
    };
    let mut rows = state
        .provider
        .list_visible_items(user.id, parent_id_for_provider)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let is_library_parent = matches!(scope, ParentScope::Library(_));
    let is_folder_id = parent.map(|pid| pid.starts_with("folder_")).unwrap_or(false);
    let wants_episodes = query
        .include_item_types
        .as_deref()
        .map(|types| types.split(',').any(|t| t.trim().eq_ignore_ascii_case("Episode")))
        .unwrap_or(false);
    let is_folder_query = query.is_folder.unwrap_or(false);
    let recursive = query.recursive.unwrap_or(true);
    if is_library_parent && (!recursive || is_folder_query) {
        if let Some(lib_id) = query.parent_id.as_deref() {
            if let Some(target_lib) = libraries.iter().find(|l| l.id == lib_id) {
                let mut folder_items: Vec<Value> = Vec::new();
                let server_id = state.provider.server_id();
                for root in &target_lib.root_paths {
                    let folder_name = root.file_name().and_then(|n| n.to_str()).unwrap_or("folder");
                    let folder_id = format!("folder_{}", uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, root.to_string_lossy().as_bytes()).simple());
                    folder_items.push(json!({
                        "Name": folder_name,
                        "ServerId": server_id,
                        "Id": folder_id,
                        "Type": "Folder",
                        "IsFolder": true,
                        "CollectionType": match target_lib.kind {
                            domain::MediaKind::Movie => "movies",
                            domain::MediaKind::Tv => "tvshows",
                            domain::MediaKind::Video => "homevideos",
                        },
                    }));
                }
                let total = folder_items.len();
                return Ok(Json(json!({ "Items": folder_items, "TotalRecordCount": total })));
            }
        }
    }
    if is_folder_id {
        if let Some(target_folder_id) = query.parent_id.as_deref() {
            rows.retain(|item| match &scope {
                // scope 已经解析出该 folder_ id 对应的库根。
                ParentScope::Folder(root) => {
                    std::path::Path::new(&item.row.path).starts_with(root)
                }
                // folder_ + SeriesId 的既有组合：provider 收到的是 folder id，
                // 这里按原逻辑兜底（结果同样是空）。
                _ => folder_id_matches(&libraries, &target_folder_id, &item.row.path),
            });
        }
    }
    if wants_episodes && rows.iter().any(|r| r.is_series) {
        let mut expanded = Vec::new();
        for item in rows {
            if item.is_series {
                let series_id = item.media.id.to_string().replace('-', "");
                if let Ok(episodes) = state.provider.list_visible_items(user.id, Some(&series_id)).await {
                    expanded.extend(episodes.into_iter().filter(|episode| {
                        if !is_library_parent {
                            return true;
                        }
                        let Some(target_library_id) = query.parent_id.as_deref() else {
                            return false;
                        };
                        libraries
                            .iter()
                            .filter(|library| library.id == target_library_id)
                            .flat_map(|library| &library.root_paths)
                            .any(|root| {
                                let target_folder_id = format!(
                                    "folder_{}",
                                    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, root.to_string_lossy().as_bytes()).simple()
                                );
                                folder_id_matches(&libraries, &target_folder_id, &episode.row.path)
                            })
                            && !libraries
                                .iter()
                                .filter(|library| library.id != target_library_id)
                                .flat_map(|library| &library.root_paths)
                                .any(|root| {
                                    let other_folder_id = format!(
                                        "folder_{}",
                                        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, root.to_string_lossy().as_bytes()).simple()
                                    );
                                    folder_id_matches(&libraries, &other_folder_id, &episode.row.path)
                                })
                    }));
                }
            } else {
                expanded.push(item);
            }
        }
        rows = expanded;
    }
    // genre/studio 判定保持既有位置（folder 过滤与分集展开之后）。认不出 genre/studio
    // 的 parent 一律返回空集合，绝不退化成整库。
    let genre_target = if needs_items {
        match classify_genre_or_studio(parent.unwrap_or_default(), &rows) {
            ParentScope::Genre(id) | ParentScope::Studio(id) => Some(id),
            _ => None,
        }
    } else {
        None
    };
    if let Some(target_id) = genre_target {
        rows.retain(|item| {
            item.metadata.genres.iter().any(|g| {
                let u = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, g.as_bytes()).hyphenated().to_string();
                let s = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, g.as_bytes()).simple().to_string();
                target_id.eq_ignore_ascii_case(&u) || target_id.eq_ignore_ascii_case(&s) || g.eq_ignore_ascii_case(&target_id)
            }) || item.metadata.studios.iter().any(|st| {
                let u = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, st.as_bytes()).hyphenated().to_string();
                let s = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, st.as_bytes()).simple().to_string();
                target_id.eq_ignore_ascii_case(&u) || target_id.eq_ignore_ascii_case(&s) || st.eq_ignore_ascii_case(&target_id)
            })
        });
    } else if needs_items {
        return Ok(Json(json!({ "Items": [], "TotalRecordCount": 0 })));
    }
    if query.person_ids.is_some() {
        let mut enriched = Vec::with_capacity(rows.len());
        for row in rows {
            enriched.push(state.provider.enrich_visible_item(row).await);
        }
        rows = enriched;
    }
    let rows = filter_items(rows, &query);
    let episodes_only = !rows.is_empty()
        && rows
            .iter()
            .all(|item| item.media.kind == domain::MediaKind::Tv && !item.is_series);
    let (default_sort, default_order) = if episodes_only {
        ("ParentIndexNumber,IndexNumber", "Ascending")
    } else {
        ("DateCreated,SortName", "Descending")
    };
    let rows = item_filters::sort_items(
        rows,
        query.sort_by.as_deref().unwrap_or(default_sort),
        query.sort_order.as_deref().unwrap_or(default_order),
    );
    let total = rows.len();
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    let listed: Vec<Value> = rows
        .into_iter()
        .skip(start_index)
        .take(limit)
        .map(item_dto_json)
        .collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": total })))
}
pub(super) async fn resume_items(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserPath>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    require_user_path(state.provider.as_ref(), user.id, path.user_id.as_deref()).await?;
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let mut rows = if query.parent_id.is_none() {
        state.provider.list_resumable_items(user.id, limit).await.unwrap_or_default()
    } else {
        let root_rows = state
            .provider
            .list_visible_items(user.id, query.parent_id.as_deref())
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut filtered = Vec::new();
        for item in root_rows {
            if item.ticks > 0 && !item.played {
                filtered.push(item);
                if filtered.len() >= limit {
                    break;
                }
            }
        }
        filtered
    };
    let total = rows.len();
    let start_index = query.start_index.unwrap_or(0);
    let listed: Vec<Value> = rows
        .drain(..)
        .skip(start_index)
        .take(limit)
        .map(item_dto_json)
        .collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": total })))
}
pub(super) async fn latest_items(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserPath>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    require_user_path(state.provider.as_ref(), user.id, path.user_id.as_deref()).await?;
    let rows = state
        .provider
        .list_visible_items(user.id, query.parent_id.as_deref())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    // 「最近入库」的播放状态口径（见 library::latest）：
    // 显式给了 IsPlayed 就按它严格筛（协议语义原样保留）；没给时**只排不筛**——
    // 没看完的在前、已看完的沉底，段内仍是入库时间倒序。
    // Jellyfin 在缺省位置会因为 HidePlayedInLatest 直接把已看完的藏掉，全看过的库
    // 更是连入口都没有；我们改成排到最后，客户端那一行于是永远有内容。
    let rows = item_filters::filter_items_except_played(rows, &query);
    let rows = item_filters::sort_items(rows, "DateCreated,SortName", "Descending");
    let rows = match query.is_played {
        Some(played) => rows
            .into_iter()
            .filter(|item| item.played == played)
            .collect(),
        None => library::played_last(rows, |item| item.played),
    };
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let listed: Vec<Value> = rows
        .into_iter()
        .skip(start_index)
        .take(limit)
        .map(item_dto_json)
        .collect();
    Ok(Json(json!(listed)))
}
pub(super) async fn genres(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let libraries = state
        .provider
        .list_visible_libraries(user.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let parent_id = library_parent(query.parent_id.as_deref(), &libraries);
    let items = state
        .provider
        .list_visible_items(user.id, parent_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut genre_counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for it in items {
        for g in it.metadata.genres {
            if !g.trim().is_empty() {
                *genre_counts.entry(g).or_insert(0) += 1;
            }
        }
    }
    let server_id = state.provider.server_id();
    let search = query.search_term.as_deref().map(|s| s.to_lowercase());
    let mut dtos = Vec::new();
    for (name, count) in genre_counts {
        if let Some(term) = &search {
            if !name.to_lowercase().contains(term) {
                continue;
            }
        }
        dtos.push(json!({
            "Name": name,
            "ServerId": server_id,
            "Id": uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes()).hyphenated().to_string(),
            "Type": "Genre",
            "IsFolder": true,
            "ChildCount": count,
            "ImageTags": { "Primary": "genre" },
            "BackdropImageTags": ["genre"],
        }));
    }
    let total = dtos.len();
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    let listed: Vec<Value> = dtos.into_iter().skip(start_index).take(limit).collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": total })))
}
pub(super) async fn genre_by_name(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let server_id = state.provider.server_id();
    Ok(Json(json!({
        "Name": name,
        "ServerId": server_id,
        "Id": uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes()).hyphenated().to_string(),
        "Type": "Genre",
        "IsFolder": true,
        "ImageTags": { "Primary": "genre" },
        "BackdropImageTags": ["genre"],
    })))
}
pub(super) async fn studios(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let libraries = state
        .provider
        .list_visible_libraries(user.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let parent_id = library_parent(query.parent_id.as_deref(), &libraries);
    let items = state
        .provider
        .list_visible_items(user.id, parent_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut studio_counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for it in items {
        for s in it.metadata.studios {
            if !s.trim().is_empty() {
                *studio_counts.entry(s).or_insert(0) += 1;
            }
        }
    }
    let server_id = state.provider.server_id();
    let search = query.search_term.as_deref().map(|s| s.to_lowercase());
    let mut dtos = Vec::new();
    for (name, count) in studio_counts {
        if let Some(term) = &search {
            if !name.to_lowercase().contains(term) {
                continue;
            }
        }
        dtos.push(json!({
            "Name": name,
            "ServerId": server_id,
            "Id": uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes()).hyphenated().to_string(),
            "Type": "Studio",
            "IsFolder": true,
            "ChildCount": count,
            "ImageTags": { "Primary": "studio" },
            "BackdropImageTags": ["studio"],
        }));
    }
    let total = dtos.len();
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(100).clamp(1, 200);
    let listed: Vec<Value> = dtos.into_iter().skip(start_index).take(limit).collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": total })))
}
pub(super) async fn studio_by_name(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let server_id = state.provider.server_id();
    Ok(Json(json!({
        "Name": name,
        "ServerId": server_id,
        "Id": uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes()).hyphenated().to_string(),
        "Type": "Studio",
        "IsFolder": true,
        "ImageTags": { "Primary": "studio" },
        "BackdropImageTags": ["studio"],
    })))
}
#[derive(Default, Deserialize)]
pub(super) struct SearchHintsQuery {
    #[serde(default, rename = "SearchTerm", alias = "searchTerm")]
    search_term: Option<String>,
    #[serde(default, rename = "StartIndex", alias = "startIndex")]
    start_index: Option<usize>,
    #[serde(default, rename = "Limit", alias = "limit")]
    limit: Option<usize>,
}
pub(super) async fn search_hints(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<SearchHintsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let Some(term) = query.search_term.map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()) else {
        return Ok(Json(json!({ "SearchHints": [], "TotalRecordCount": 0 })));
    };
    let items = state
        .provider
        .list_visible_items(user.id, None)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut hints = Vec::new();
    for it in items {
        let title_match = it.media.title.to_lowercase().contains(&term);
        let orig_match = it.metadata.original_title.as_deref().map(|s| s.to_lowercase().contains(&term)).unwrap_or(false);
        if title_match || orig_match {
            let id = if it.is_series {
                it.media.id.to_string().replace('-', "")
            } else {
                it.row.id.to_string().replace('-', "")
            };
            let item_type = if it.is_series {
                "Series"
            } else if it.media.kind == domain::MediaKind::Tv {
                "Episode"
            } else {
                "Movie"
            };
            hints.push(json!({
                "ItemId": id,
                "Id": id,
                "Name": it.media.title,
                "Type": item_type,
                "IsFolder": it.is_series,
                "RunTimeTicks": it.metadata.runtime_ticks,
                "ProductionYear": it.metadata.production_year,
                "MatchedTerm": term,
            }));
        }
    }
    let total = hints.len();
    let start_index = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let listed: Vec<Value> = hints.into_iter().skip(start_index).take(limit).collect();
    Ok(Json(json!({ "SearchHints": listed, "TotalRecordCount": total })))
}
pub(super) async fn item_by_id(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(path): Path<UserItemPath>,
) -> Result<Json<Value>, StatusCode> {
    let id = item_id_for_user(state.provider.as_ref(), path, user.id).await?;
    let snapshot = state
        .provider
        .resolve_single_item(user.id, &id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(item_dto_json(snapshot)))
}

/// `folder_<uuid5(root)>` 形式的历史兜底匹配（folder_ + SeriesId 组合走这条路径）。
fn folder_id_matches(
    libraries: &[crate::provider::ServerLibrary],
    target: &str,
    path: &str,
) -> bool {
    libraries.iter().any(|lib| {
        lib.root_paths.iter().any(|root| {
            let f_id = format!(
                "folder_{}",
                uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, root.to_string_lossy().as_bytes())
                    .simple()
            );
            f_id == target && std::path::Path::new(path).starts_with(root)
        })
    })
}
