use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::http::library::{page, preferred_row, require_visible_library, selection};
use crate::http::{err, ok_list};
use crate::management::ApiState;

pub(crate) async fn gallery(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    // 显式分页参数先校验：越界直接 400；省略 limit 仍是"返回全部"。
    let page = match page::PageWindow::parse(&query) {
        Ok(page) => page,
        Err(message) => {
            return err(
                axum::http::StatusCode::BAD_REQUEST,
                "library.invalid",
                &message,
            );
        }
    };
    // 授权从当前配置判定；图片事实优先取目标 Library 的 ready 投影（零文件 IO），
    // 投影不可用时回退到旧的全量读取（已在 helper 内 warn）。
    let library = {
        let store = state.store.lock();
        match require_visible_library(&store, &id, user_id) {
            Ok(library) => library,
            Err(response) => return response,
        }
    };
    let (snapshot, artwork) =
        match selection::load_browse_snapshot_or_projection(&state, &library, user_id).await {
            Ok(loaded) => loaded,
            Err(response) => return response,
        };
    let files = state.browse_files.clone();
    let expansions = state.gallery_expansions.clone();
    let work = state
        .http_work
        .run(selection::BROWSE_DEADLINE, move || {
            let library_id = snapshot.library_id.clone();
            let selected = match selection::select_snapshot(snapshot, &query, files.as_ref()) {
                Ok(items) => items,
                Err(response) => return response,
            };
            let art = selection::BrowseArtwork::new(artwork.as_ref(), files.as_ref());
            // 先按图片存在性剔除空组、再分页：只扫描到填满本页窗口为止，
            // 且只为本页构建图片 JSON（不再为每个过滤后的 Media 都建一遍）。
            let page_items = gallery_page_items(&selected, page, &art);
            let mut groups = Vec::with_capacity(page_items.len());
            for item in page_items {
                expansions.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let images = gallery_images(&library_id, &item.media, &item.rows, &art);
                if images.is_empty() {
                    continue;
                }
                groups.push(json!({
                    "library_id": library_id,
                    "media_item_id": item.media.id.to_string(),
                    "title": item.media.title,
                    "is_favorite": item.favorite,
                    "images": images,
                }));
            }
            ok_list(groups).into_response()
        })
        .await;
    match work {
        Ok(response) => response,
        Err(error) => selection::browse_work_error(error),
    }
}

/// 先按图片存在性剔除空组，再取分页窗口。
///
/// 只扫描到填满 `offset + limit` 个非空组为止（省略 limit 时扫描全部），因此
/// 空组不会造成短页，而图集 JSON 只为本页构建。旧实现为每个过滤后的 Media 都
/// 先建完整图片数组再分页，本函数把该成本压到页大小。
fn gallery_page_items<'a>(
    selected: &'a [selection::LibrarySelection],
    page: page::PageWindow,
    art: &selection::BrowseArtwork<'_>,
) -> Vec<&'a selection::LibrarySelection> {
    let end = match page.limit {
        Some(limit) => page.offset.saturating_add(limit),
        None => usize::MAX,
    };
    let mut out = Vec::new();
    let mut non_empty_seen = 0usize;
    for item in selected {
        if !has_gallery_images(item, art) {
            continue;
        }
        if non_empty_seen >= page.offset {
            out.push(item);
        }
        non_empty_seen += 1;
        if non_empty_seen >= end {
            break;
        }
    }
    out
}

/// 与 [`gallery_images`] 同一图片存在性口径：海报 / 背景 / 任一单元剧照。
fn has_gallery_images(
    item: &selection::LibrarySelection,
    art: &selection::BrowseArtwork<'_>,
) -> bool {
    let facts = art.media(item.media.id, &item.rows);
    facts.has_poster
        || facts.has_backdrop
        || item.rows.iter().any(|row| art.has_still(row))
}

/// 图组里的一类图片。每类自带 URL 前缀、宽高比与展示名，调用方不必再传一
/// 串彼此绑定的字符串/数字参数。
#[derive(Clone, Copy)]
enum GalleryImageKind {
    Poster,
    Backdrop,
    Still,
}

impl GalleryImageKind {
    fn url_kind(self) -> &'static str {
        match self {
            Self::Poster => "posters",
            Self::Backdrop => "fanart",
            Self::Still => "stills",
        }
    }

    fn aspect(self) -> f64 {
        match self {
            Self::Poster => 0.667,
            Self::Backdrop | Self::Still => 1.778,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Poster => "poster",
            Self::Backdrop => "backdrop",
            Self::Still => "still",
        }
    }
}

fn gallery_images(
    library_id: &str,
    media: &domain::Media,
    rows: &[domain::LedgerRow],
    art: &selection::BrowseArtwork<'_>,
) -> Vec<Value> {
    let refs: Vec<&domain::LedgerRow> = rows.iter().collect();
    let Some(pref_row) = preferred_row(&refs) else {
        return Vec::new();
    };
    let facts = art.media(media.id, rows);
    let mut images = Vec::new();

    if facts.has_poster {
        push_image(
            &mut images,
            library_id,
            media,
            pref_row,
            GalleryImageKind::Poster,
            None,
            None,
        );
    }
    if facts.has_backdrop {
        push_image(
            &mut images,
            library_id,
            media,
            pref_row,
            GalleryImageKind::Backdrop,
            None,
            None,
        );
    }

    // 遍历所有有剧照的单元（剧照存在性来自投影 / 注入 seam，不再现场 stat）。
    for row in rows {
        if art.has_still(row) {
            push_image(
                &mut images,
                library_id,
                media,
                row,
                GalleryImageKind::Still,
                row.season,
                row.episode,
            );
        }
    }
    images
}

fn push_image(
    images: &mut Vec<Value>,
    library_id: &str,
    media: &domain::Media,
    row: &domain::LedgerRow,
    kind: GalleryImageKind,
    season: Option<u32>,
    episode: Option<u32>,
) {
    let label = match kind {
        GalleryImageKind::Poster => "海报".to_string(),
        GalleryImageKind::Backdrop => "背景".to_string(),
        GalleryImageKind::Still => match (season, episode) {
            (Some(s), Some(e)) => format!("第 {s} 季 第 {e} 集"),
            _ => "剧照".to_string(),
        },
    };
    images.push(json!({
        "url": crate::http::library::artwork_url(kind.url_kind(), row.id),
        "aspect": kind.aspect(),
        "kind": kind.name(),
        "alt": media.title,
        "media_item_id": media.id.to_string(),
        "library_id": library_id,
        "season": season,
        "episode": episode,
        "label": label,
        "t_seconds": Value::Null,
    }));
}
