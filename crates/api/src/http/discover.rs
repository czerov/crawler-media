//! Discover: catalog rows for movie / TV walls, optionally one source.
//! Section table mirrors the upstream discover page (20 movie + 20 TV rows).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use media::CatalogHit;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::blocking_budget::BlockingError;
use crate::catalog::Catalog;
use crate::http::{err, ok};
use crate::management::ApiState;

mod sections;

use self::sections::{
    collection_title, douban_movie_sections, douban_tv_sections, movie_sections, section_query,
    supports_full_listing, tv_sections,
};

const TMDB_POSTER: &str = "https://image.tmdb.org/t/p/w342";
const TMDB_BACKDROP: &str = "https://image.tmdb.org/t/p/w1280";
/// 只读 catalog 墙的总等待预算；不替代 catalog 自身的 HTTP 超时。
const DISCOVER_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// Runtime failures of the blocking budget (not upstream catalog errors).
fn blocking_error(error: BlockingError) -> Response {
    match error {
        BlockingError::Busy => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请重试",
        ),
        BlockingError::Timeout => err(
            StatusCode::GATEWAY_TIMEOUT,
            "discover.timeout",
            "分区加载超时",
        ),
        error => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "discover.worker",
            &error.to_string(),
        ),
    }
}

#[derive(Deserialize)]
pub(crate) struct DiscoverQuery {
    /// "tmdb" | "douban" | absent = all sources merged.
    source: Option<String>,
}

pub(crate) async fn discover_kind(
    State(state): State<ApiState>,
    Path(kind): Path<String>,
    Query(query): Query<DiscoverQuery>,
) -> Response {
    // 分区墙一次串行拉 20 个分区，每个都是同步 TMDB/豆瓣 HTTP 请求
    //（ureq 阻塞 1-8s）。直接在 async handler 里跑会占满 tokio 工作线程，
    // 其他请求（包括无锁的 /health）全部排队超时——表现为「正在连接服务…」。
    // 挪到有界 blocking 池，worker 线程不被占用。
    // 错误码区分：未知 kind/source 是客户端错（400），上游失败是 502。
    let state_clone = state.clone();
    let kind_clone = kind.clone();
    let source = query.source.clone();
    let sections = state
        .http_work
        .run(DISCOVER_DEADLINE, move || -> Result<Vec<Value>, (u16, String)> {
            let mut catalog: &dyn Catalog = state_clone.catalog.as_ref();
            if let Some(source) = source.as_deref().filter(|source| !source.is_empty()) {
                match catalog.source(source) {
                    Some(found) => catalog = found,
                    None if catalog.source_name() == source => {}
                    None => return Err((400, format!("未知数据源: {source}"))),
                }
            }
            match kind_clone.as_str() {
                // 豆瓣源用独立分区表（真实豆瓣 tag），不走 TMDB 分区映射。
                "movie" if catalog.source_name() == "douban" => Ok(douban_movie_sections(catalog)),
                "tv" if catalog.source_name() == "douban" => Ok(douban_tv_sections(catalog)),
                "movie" => Ok(movie_sections("movie", catalog)),
                "tv" => Ok(tv_sections("tv", catalog)),
                _ => Err((400, "kind 必须是 movie 或 tv".into())),
            }
        })
        .await;
    let sections = match sections {
        Ok(Ok(sections)) => sections,
        Ok(Err((status, message))) => {
            return err(
                StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                "discover.upstream",
                &message,
            );
        }
        Err(error) => return blocking_error(error),
    };
    let tmdb_configured = state
        .store
        .lock()
        .get_setting(crate::settings_keys::TMDB_API_KEY)
        .ok()
        .flatten()
        .map(|value| !value.is_empty())
        .unwrap_or(false);
    ok(json!({
        "kind": kind,
        "tmdb_configured": tmdb_configured,
        "sections": sections,
    }))
    .into_response()
}

/// One wall row: (kind, id, title, presentation, items).
fn row(
    source: &str,
    kind: &str,
    id: &str,
    title: &str,
    presentation: &str,
    result: Result<Vec<CatalogHit>, String>,
) -> Value {
    match result {
        Ok(hits) => json!({
            "id": id,
            "title": title,
            "presentation": presentation,
            "supports_full_listing": supports_full_listing(source, kind, id),
            "items": hits.into_iter().map(item_json).collect::<Vec<_>>(),
        }),
        Err(error) => {
            tracing::warn!(source, kind, section = id, %error, "发现墙单分区加载失败，优雅降级");
            json!({
                "id": id,
                "title": title,
                "presentation": presentation,
                "supports_full_listing": supports_full_listing(source, kind, id),
                "items": [],
                "error": {
                    "code": "discover.section_failed",
                    "message": "分区暂时无法加载，请重试"
                },
            })
        }
    }
}

fn item_json(hit: CatalogHit) -> Value {
    let id = hit
        .media
        .tmdb_id
        .clone()
        .or(hit.media.douban_id.clone())
        .unwrap_or_else(|| hit.media.id.to_string());
    json!({
        "id": id,
        "title": hit.media.title,
        "year": hit.media.year,
        "kind": hit.media.kind.as_str(),
        "tmdb_id": hit.media.tmdb_id,
        "poster_url": poster_url(hit.poster_path.as_deref()),
        "backdrop_url": backdrop_url(hit.backdrop_path.as_deref()),
        "rating": hit.rating,
        "overview": hit.overview,
    })
}

fn poster_url(path: Option<&str>) -> Option<String> {
    remote_or_prefixed(path, TMDB_POSTER)
}

fn backdrop_url(path: Option<&str>) -> Option<String> {
    remote_or_prefixed(path, TMDB_BACKDROP)
}

fn remote_or_prefixed(path: Option<&str>, prefix: &str) -> Option<String> {
    let path = path.filter(|path| !path.is_empty())?;
    if path.starts_with("http://") || path.starts_with("https://") {
        Some(path.to_string())
    } else if path.starts_with('/') {
        Some(format!("{prefix}{path}"))
    } else {
        None
    }
}

#[derive(Deserialize, Default)]
pub(crate) struct FilterQuery {
    genres: Option<String>,
    country: Option<String>,
    year: Option<u32>,
    rating: Option<f32>,
    runtime: Option<u32>,
    sort: Option<String>,
    page: Option<i64>,
}

/// Filtered wall: `GET /discover/{kind}/filtered?genres=..&country=..&year=..
/// &rating=..&runtime=..&sort=..`. Only TMDB sources implement real filters;
/// other unsupported sources are safely ignored in fanout merge, returning empty list (200) instead of 502.
pub(crate) async fn discover_filtered(
    State(state): State<ApiState>,
    Path(kind): Path<String>,
    Query(query): Query<FilterQuery>,
) -> Response {
    let mut params = Vec::new();
    if let Some(genres) = query.genres.as_deref().filter(|value| !value.is_empty()) {
        params.push(format!("with_genres={genres}"));
    }
    // TV 且用户没指定类型时，默认排除综艺/真人秀（TMDB 的 CN origin 大量是
    // 这些，用户筛「中国剧」时几乎总是想要剧情剧而不是《奔跑吧》）。
    if kind.as_str() == "tv" && query.genres.as_deref().unwrap_or("").is_empty() {
        params.push("without_genres=10764,10767".to_string());
    }
    if let Some(country) = query.country.as_deref().filter(|value| !value.is_empty()) {
        params.push(format!("with_origin_country={country}"));
    }
    let is_tv = kind.as_str() == "tv";
    let sort = match query.sort.as_deref() {
        Some("rating") => "vote_average.desc",
        // TV 的「最新」按首播日，电影按上映日；用错字段 TMDB 会忽略条件。
        Some("newest") => {
            if is_tv {
                "first_air_date.desc"
            } else {
                "primary_release_date.desc"
            }
        }
        Some("most-rated") => "vote_count.desc",
        _ => "popularity.desc",
    };
    params.push(format!("sort_by={sort}"));
    if let Some(year) = query.year {
        // TV 没有 primary_release_year，只有 first_air_date_year。
        if is_tv {
            params.push(format!("first_air_date_year={year}"));
        } else {
            params.push(format!("primary_release_year={year}"));
        }
    }
    if let Some(rating) = query.rating {
        params.push(format!("vote_average.gte={rating}"));
    }
    if let Some(runtime) = query.runtime {
        params.push(format!("with_runtime.lte={runtime}"));
    }
    let query_string = params.join("&");
    let page = query.page.unwrap_or(1).max(1);
    let state_clone = state.clone();
    let kind_clone = kind.clone();
    let qs_clone = query_string.clone();
    let paged_res = state
        .http_work
        .run(
            DISCOVER_DEADLINE,
            move || -> Result<(Vec<CatalogHit>, i64, i64), (u16, String)> {
                let catalog: &dyn Catalog = state_clone.catalog.as_ref();
                match kind_clone.as_str() {
                    "movie" => {
                        match catalog.collection_paged(domain::MediaKind::Movie, &qs_clone, page) {
                            Ok(Some((hits, _, tp, tr))) => Ok((hits, tp, tr)),
                            _ => {
                                let hits =
                                    catalog.filtered_movie(&qs_clone).map_err(|e| (502, e))?;
                                let len = hits.len() as i64;
                                Ok((hits, if len > 0 { 1 } else { 0 }, len))
                            }
                        }
                    }
                    "tv" => match catalog.collection_paged(domain::MediaKind::Tv, &qs_clone, page)
                    {
                        Ok(Some((hits, _, tp, tr))) => Ok((hits, tp, tr)),
                        _ => {
                            let hits = catalog.filtered_tv(&qs_clone).map_err(|e| (502, e))?;
                            let len = hits.len() as i64;
                            Ok((hits, if len > 0 { 1 } else { 0 }, len))
                        }
                    },
                    _ => Err((400, "kind 必须是 movie 或 tv".into())),
                }
            },
        )
        .await;

    let (hits, total_pages, total_results) = match paged_res {
        Ok(Ok(res)) => res,
        Ok(Err((status, message))) => {
            return err(
                StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                "discover.upstream",
                &message,
            );
        }
        Err(error) => return blocking_error(error),
    };
    ok(json!({
        "kind": kind,
        "page": page,
        "total_pages": total_pages,
        "total_results": total_results,
        "has_more": page < total_pages,
        "items": hits.into_iter().map(item_json).collect::<Vec<_>>(),
    }))
    .into_response()
}

/// `GET /discover/{kind}/collection/{id}?source=&page=1&page_size=20` — a full
/// listing for one wall section (分页). 404 when the section has no listing.
/// source=douban 走豆瓣翻页（仅可翻页分区有入口），否则 TMDB discover。
pub(crate) async fn discover_collection(
    State(state): State<ApiState>,
    Path((kind, id)): Path<(String, String)>,
    Query(query): Query<CollectionQuery>,
) -> Response {
    let media_kind = match kind.as_str() {
        "movie" => domain::MediaKind::Movie,
        "tv" => domain::MediaKind::Tv,
        _ => {
            return err(
                StatusCode::BAD_REQUEST,
                "discover.kind",
                "kind 必须是 movie 或 tv",
            );
        }
    };
    let mut catalog: &dyn Catalog = state.catalog.as_ref();
    if let Some(source) = query.source.as_deref().filter(|source| !source.is_empty()) {
        match catalog.source(source) {
            Some(found) => catalog = found,
            None if catalog.source_name() == source => {}
            None => return err(StatusCode::BAD_REQUEST, "discover.source", "未知数据源"),
        }
    }
    let is_douban = catalog.source_name() == "douban";
    let normalized_id = match id.as_str() {
        "movie_high_score" | "movie_top250" => "top-rated",
        other => other,
    };
    // 豆瓣源只有高分分区有完整榜单；其他分区（走 TMDB query 或豆瓣无翻页 tag）拒绝。
    if is_douban && normalized_id != "top-rated" {
        return err(
            StatusCode::NOT_FOUND,
            "discover.collection",
            "该分区不支持豆瓣完整榜单",
        );
    }
    let Some(section_query) = section_query(&kind, normalized_id) else {
        return err(
            StatusCode::NOT_FOUND,
            "discover.collection",
            "该分区不支持完整榜单",
        );
    };
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(20).clamp(10, 100);
    let state_clone = state.clone();
    let media_kind_clone = media_kind;
    let source_clone = query.source.clone();
    let paged = state
        .http_work
        .run(DISCOVER_DEADLINE, move || {
            let mut catalog: &dyn Catalog = state_clone.catalog.as_ref();
            if let Some(source) = source_clone.as_deref().filter(|source| !source.is_empty()) {
                if let Some(found) = catalog.source(source) {
                    catalog = found;
                }
            }
            catalog.collection_paged(media_kind_clone, &section_query, page)
        })
        .await;
    let paged = match paged {
        Ok(Ok(Some(result))) => result,
        Ok(Ok(None)) => {
            return err(
                StatusCode::NOT_FOUND,
                "discover.collection",
                "该分区不支持完整榜单",
            );
        }
        Ok(Err(error)) => return err(StatusCode::BAD_GATEWAY, "discover.upstream", &error),
        Err(error) => return blocking_error(error),
    };
    let (hits, _, total_pages, total_results) = paged;
    let items: Vec<Value> = hits
        .into_iter()
        .take(page_size as usize)
        .map(item_json)
        .collect();
    ok(json!({
        "kind": kind,
        "collection_id": normalized_id,
        "title": collection_title(&kind, normalized_id),
        "items": items,
        "page": page,
        "total_pages": total_pages,
        "total_results": total_results,
        "has_more": page < total_pages,
    }))
    .into_response()
}

#[derive(Deserialize, Default)]
pub(crate) struct CollectionQuery {
    source: Option<String>,
    page: Option<i64>,
    page_size: Option<i64>,
}

/// `GET /genres/{kind}` — filter dialog needs the TMDB genre list.
pub(crate) async fn genres(State(state): State<ApiState>, Path(kind): Path<String>) -> Response {
    let media_kind = match kind.as_str() {
        "movie" => domain::MediaKind::Movie,
        "tv" => domain::MediaKind::Tv,
        _ => {
            return err(
                StatusCode::BAD_REQUEST,
                "discover.kind",
                "kind 必须是 movie 或 tv",
            );
        }
    };
    let state_clone = state.clone();
    let rows = state
        .http_work
        .run(DISCOVER_DEADLINE, move || state_clone.catalog.genres(media_kind))
        .await;
    let rows = match rows {
        Ok(Ok(rows)) => rows,
        Ok(Err(error)) => return err(StatusCode::BAD_GATEWAY, "discover.upstream", &error),
        Err(error) => return blocking_error(error),
    };
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, name)| json!({ "id": id, "name": name }))
        .collect();
    ok(json!({ "kind": kind, "genres": items })).into_response()
}
