use std::collections::HashMap;
use std::path::Path as FsPath;

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
    // 锁内只取 owned 快照；NFO/stat/过滤/排序/渲染都在锁外的 blocking 预算内。
    let snapshot = {
        let store = state.store.lock();
        let library = match require_visible_library(&store, &id, user_id) {
            Ok(library) => library,
            Err(response) => return response,
        };
        match selection::load_browse_snapshot(&store, &library, user_id) {
            Ok(snapshot) => snapshot,
            Err(response) => return response,
        }
    };
    let files = state.browse_files.clone();
    let work = state
        .http_work
        .run(selection::BROWSE_DEADLINE, move || {
            let library_id = snapshot.library_id.clone();
            let selected = match selection::select_snapshot(snapshot, &query, files.as_ref()) {
                Ok(items) => items,
                Err(response) => return response,
            };
            let mut groups = Vec::new();
            for item in selected {
                let images = gallery_images(&library_id, &item.media, &item.rows);
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
            // 空图组已在上面剔除，分页必须发生在剔除之后；bounds 对 offset+limit 溢出免疫。
            let bounds = page.bounds(groups.len());
            ok_list(groups[bounds].to_vec()).into_response()
        })
        .await;
    match work {
        Ok(response) => response,
        Err(error) => selection::browse_work_error(error),
    }
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
) -> Vec<Value> {
    let refs: Vec<&domain::LedgerRow> = rows.iter().collect();
    let Some(pref_row) = preferred_row(&refs) else {
        return Vec::new();
    };
    let video = FsPath::new(&pref_row.path);
    let Some(dir) = video.parent() else {
        return Vec::new();
    };
    let parent_dir = dir.parent().unwrap_or(dir);
    let mut images = Vec::new();

    let poster_path = if dir.join("poster.jpg").is_file() {
        Some(dir.join("poster.jpg"))
    } else if parent_dir.join("poster.jpg").is_file() {
        Some(parent_dir.join("poster.jpg"))
    } else {
        None
    };
    if let Some(p) = poster_path {
        push_image(
            &mut images,
            library_id,
            media,
            pref_row,
            GalleryImageKind::Poster,
            p,
            None,
            None,
        );
    }

    let fanart_path = if dir.join("fanart.jpg").is_file() {
        Some(dir.join("fanart.jpg"))
    } else if parent_dir.join("fanart.jpg").is_file() {
        Some(parent_dir.join("fanart.jpg"))
    } else {
        None
    };
    if let Some(p) = fanart_path {
        push_image(
            &mut images,
            library_id,
            media,
            pref_row,
            GalleryImageKind::Backdrop,
            p,
            None,
            None,
        );
    }

    // 遍历所有有剧照的单元
    for row in rows {
        let vpath = FsPath::new(&row.path);
        if crate::episode_still::existing(vpath).is_some() {
            push_image(
                &mut images,
                library_id,
                media,
                row,
                GalleryImageKind::Still,
                vpath.to_path_buf(),
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
    path: std::path::PathBuf,
    season: Option<u32>,
    episode: Option<u32>,
) {
    // Stills live inside the container, so only the filesystem artwork has to exist.
    if !matches!(kind, GalleryImageKind::Still) && !path.is_file() {
        return;
    }
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
