//! Jellyfin/Emby shows: NextUp, seasons and episodes.

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{Value, json};

use crate::auth::AuthUser;
use crate::dto::item_dto_json;

use super::{AppState, ItemsQuery, parse_season_id};

pub(super) async fn shows_next_up(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let offset = query.start_index.unwrap_or(0);
    // 一次聚合：候选按 Media 分组、每个 Media 取第一条未看，再整体分页。读取失败
    // 必须变成错误响应，绝不能被吞成「空的一页」。
    let page = state
        .provider
        .next_up_items(user.id, query.series_id.as_deref(), offset, limit)
        .await
        .map_err(|error| {
            tracing::error!(%error, user_id = %user.id, "Jellyfin NextUp 聚合失败");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let listed: Vec<Value> = page.items.into_iter().map(item_dto_json).collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": page.total })))
}
pub(super) async fn show_seasons(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let item = state
        .provider
        .resolve_single_item(user.id, &id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let compact_id = item.media.id.to_string().replace('-', "");
    let episodes = state
        .provider
        .list_visible_items(user.id, Some(&compact_id))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut season_numbers: std::collections::BTreeSet<u32> = episodes
        .iter()
        .map(|episode| episode.row.season.unwrap_or(1))
        .collect();
    if season_numbers.is_empty() {
        season_numbers.insert(item.row.season.unwrap_or(1));
    }
    let server_id = state.provider.server_id();
    let mut seasons_json: Vec<Value> = Vec::new();
    for season_num in season_numbers {
        let season_id = format!("{compact_id}{season_num:02}");
        let season_episodes: Vec<&crate::provider::MediaItemSnapshot> = episodes.iter().filter(|e| e.row.season == Some(season_num)).collect();
        let played_count = season_episodes.iter().filter(|e| e.played).count();
        let season_total = season_episodes.len();
        let unplayed = season_total.saturating_sub(played_count);
        let user_data = json!({
            "PlaybackPositionTicks": 0,
            "PlayCount": 0,
            "IsFavorite": false,
            "Played": season_total > 0 && unplayed == 0,
            "UnplayedItemCount": unplayed,
            "PlayedPercentage": if season_total > 0 { played_count as f64 / season_total as f64 * 100.0 } else { 0.0 },
        });
        let primary_tag = item.metadata.primary_image_tag.clone();
        seasons_json.push(json!({
            "Id": season_id,
            "Name": format!("第 {} 季", season_num),
            "SeriesId": compact_id,
            "SeriesName": item.media.title,
            "IndexNumber": season_num,
            "Type": "Season",
            "IsFolder": true,
            "ServerId": server_id,
            "UserData": user_data,
            "SeriesPrimaryImageTag": primary_tag,
            "ImageTags": {
                "Primary": primary_tag.unwrap_or_else(|| "poster".to_string())
            },
            "BackdropImageTags": [],
        }));
    }
    Ok(Json(
        json!({ "Items": seasons_json, "TotalRecordCount": seasons_json.len() }),
    ))
}
pub(super) async fn show_episodes(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
    Query(query): Query<ItemsQuery>,
) -> Result<Json<Value>, StatusCode> {
    let compact_id = id.replace('-', "");
    let rows = state
        .provider
        .list_visible_items(user.id, Some(&compact_id))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let season_number = query
        .season_id
        .as_deref()
        .and_then(|season_id| parse_season_id(&compact_id, season_id));
    let limit = query.limit.unwrap_or(200);
    let mut rows: Vec<_> = rows
        .into_iter()
        .filter(|episode| match season_number {
            Some(season) => episode.row.season == Some(season),
            None => query.season_id.is_none(),
        })
        .collect();
    rows.sort_by_key(|episode| {
        (
            episode.row.season.unwrap_or_default(),
            episode.row.episode.unwrap_or_default(),
        )
    });
    let total = rows.len();
    let listed: Vec<Value> = rows
        .into_iter()
        .skip(query.start_index.unwrap_or(0))
        .take(limit)
        .map(item_dto_json)
        .collect();
    Ok(Json(json!({ "Items": listed, "TotalRecordCount": total })))
}
