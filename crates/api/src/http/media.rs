//! Unified media detail (`GET /api/v1/media/{kind}/{id}`): one endpoint that
//! powers the discover-page detail view (overview / rating / cast / backdrops /
//! related / in-library links). Douban ids fall back to TMDB detail by lookup.

use crate::blocking_budget::BlockingError;
use crate::catalog::Catalog;
use crate::http::{err, ok};
use crate::management::ApiState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

const TMDB_POSTER: &str = "https://image.tmdb.org/t/p/w342";
const TMDB_BACKDROP_SIZE: &str = "w1280";

/// 只读 catalog 详情/影人请求的等待预算；不替代 catalog 自身的 HTTP 超时。
const MEDIA_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// Blocking-budget failure → HTTP response（上游错误走各自原有路径）。
fn blocking_error(error: BlockingError, code: &'static str) -> Response {
    match error {
        BlockingError::Busy => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请重试",
        ),
        BlockingError::Timeout => err(StatusCode::GATEWAY_TIMEOUT, code, "媒体请求超时"),
        error => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "runtime.worker",
            &error.to_string(),
        ),
    }
}

pub(crate) async fn media_detail(
    State(state): State<ApiState>,
    Path((kind, id)): Path<(String, String)>,
) -> Response {
    let state_for_work = state.clone();
    let work = state
        .http_work
        .run(MEDIA_DEADLINE, move || {
            media_detail_inner(&state_for_work, &kind, &id)
        })
        .await;
    match work {
        Ok(response) => response,
        Err(error) => blocking_error(error, "media.timeout"),
    }
}

pub(crate) async fn season_detail(
    State(state): State<ApiState>,
    Path((id, season)): Path<(String, u32)>,
) -> Response {
    let state_for_work = state.clone();
    let work = state
        .http_work
        .run(MEDIA_DEADLINE, move || -> Response {
            let catalog: &dyn Catalog = state_for_work.catalog.as_ref();
            let tmdb_id = match tmdb_id_for(&state_for_work, catalog, domain::MediaKind::Tv, &id) {
                Ok(Some(tid)) => tid,
                Ok(None) => return err(StatusCode::NOT_FOUND, "media.missing", "条目不存在"),
                Err(response) => return response,
            };
            let meta_episodes = catalog.season_details(&tmdb_id, season).unwrap_or_default();
            match catalog.season_episodes(&tmdb_id, season) {
                Ok(episodes) => {
                    let items: Vec<Value> = episodes
                        .into_iter()
                        .map(|ep| {
                            let ep_num = ep.episode_number;
                            let meta = meta_episodes.iter().find(|m| m.episode_number == ep_num);
                            json!({
                                "episode_number": ep_num,
                                "name": meta.and_then(|m| m.name.clone()),
                                "overview": meta.and_then(|m| m.overview.clone()),
                                "still_url": meta.and_then(|m| m.still_path.as_deref().map(|p| format!("https://image.tmdb.org/t/p/w300{p}"))),
                                "air_date": ep.air_date,
                            })
                        })
                        .collect();
                    ok(json!({ "episodes": items })).into_response()
                }
                Err(error) => err(StatusCode::BAD_GATEWAY, "media.upstream", &error),
            }
        })
        .await;
    match work {
        Ok(response) => response,
        Err(error) => blocking_error(error, "media.timeout"),
    }
}

fn find_library_links(store: &crate::Store, media: &domain::Media) -> Vec<Value> {
    let canonical_id = media
        .tmdb_id
        .as_deref()
        .and_then(|id| {
            store
                .get_media_by_alias_kind("tmdb_id", id, Some(media.kind))
                .ok()
                .flatten()
        })
        .map(|m| m.id)
        .unwrap_or(media.id);
    let mut links = Vec::new();
    for row in store.list_ledger().unwrap_or_default() {
        if row.media_id == canonical_id || row.media_id == media.id {
            if let Some(owner) = store
                .get_media(row.media_id)
                .ok()
                .flatten()
                .and_then(|m| crate::http::library::library_for_row(store, &row, &m))
            {
                if !links
                    .iter()
                    .any(|link: &Value| link["library_id"] == owner.id)
                {
                    links.push(json!({
                        "library_id": owner.id,
                        "library_name": owner.name,
                        "media_item_id": row.media_id.to_string(),
                    }));
                }
            }
        }
    }
    links
}

fn fetch_tv_seasons(
    catalog: &dyn Catalog,
    media_kind: domain::MediaKind,
    tmdb_id: &str,
) -> Vec<Value> {
    if media_kind == domain::MediaKind::Tv {
        catalog
            .tv_seasons(tmdb_id)
            .unwrap_or_default()
            .into_iter()
            .map(|s| {
                let s_num = s.season_number;
                json!({
                    "season_number": s_num,
                    "name": s.name,
                    "episode_count": s.episode_count,
                    "air_date": s.air_date,
                })
            })
            .collect()
    } else {
        Vec::new()
    }
}

fn fetch_cast(meta: &Option<media::ItemMeta>) -> Vec<Value> {
    meta.as_ref()
        .map(|m| {
            m.cast
                .iter()
                .map(|c| {
                    json!({
                        "name": c.name,
                        "role": c.role,
                        "tmdb_person_id": c.person_id,
                        "avatar_url": c.avatar_path.as_deref().map(|path| remote_or_prefixed(Some(path), "https://image.tmdb.org/t/p/w185")),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn fetch_images_and_related(
    catalog: &dyn Catalog,
    media_kind: domain::MediaKind,
    tmdb_id: &str,
) -> (
    Vec<Value>,
    Vec<Value>,
    Option<String>,
    Option<String>,
    Vec<Value>,
) {
    let candidates = catalog.image_candidates(media_kind, tmdb_id).ok();
    let posters: Vec<Value> = candidates
        .as_ref()
        .map(|c| c.posters.iter().map(image_json).collect())
        .unwrap_or_default();
    let backdrops: Vec<Value> = candidates
        .as_ref()
        .map(|c| c.backdrops.iter().map(image_json).collect())
        .unwrap_or_default();
    let poster_url = catalog
        .poster_url(media_kind, tmdb_id)
        .ok()
        .flatten()
        .map(|url| tmdb_image_url_at_size(&url, "w780"))
        .or_else(|| {
            candidates
                .as_ref()
                .and_then(|images| images.posters.first())
                .map(|image| image.url("w780"))
        });
    let backdrop_url = candidates
        .as_ref()
        .and_then(|images| images.backdrops.first())
        .map(|image| image.url(TMDB_BACKDROP_SIZE));
    let related: Vec<Value> = match media_kind {
        domain::MediaKind::Movie => catalog.popular_movie(),
        _ => catalog.popular_tv(),
    }
    .map(|hits| {
        hits.into_iter()
            .filter(|hit| hit.media.tmdb_id.as_deref() != Some(tmdb_id))
            .take(10)
            .map(|hit| item_json(&hit.media, hit.poster_path.as_deref()))
            .collect()
    })
    .unwrap_or_default();
    (posters, backdrops, poster_url, backdrop_url, related)
}

fn media_detail_inner(state: &ApiState, kind: &str, id: &str) -> Response {
    let media_kind = match kind {
        "movie" => domain::MediaKind::Movie,
        "tv" => domain::MediaKind::Tv,
        _ => {
            return err(
                StatusCode::BAD_REQUEST,
                "media.kind",
                "kind 必须是 movie 或 tv",
            );
        }
    };
    let catalog: &dyn Catalog = state.catalog.as_ref();
    let tmdb_id = match tmdb_id_for(state, catalog, media_kind, id) {
        Ok(Some(tid)) => tid,
        Ok(None) => return err(StatusCode::NOT_FOUND, "media.missing", "条目不存在"),
        Err(response) => return response,
    };

    let media = match catalog.details(media_kind, &tmdb_id) {
        Ok(Some(media)) => media,
        Ok(None) => return err(StatusCode::NOT_FOUND, "media.missing", "条目不存在"),
        Err(error) => {
            if error.contains("404") {
                return empty_detail(kind, id);
            }
            return err(StatusCode::BAD_GATEWAY, "media.upstream", &error);
        }
    };
    let meta = match crate::scrape_metadata::fetch_tmdb_metadata(state, media_kind, &tmdb_id) {
        Ok(meta) => meta,
        Err(error) => {
            tracing::warn!(%error, %tmdb_id, "failed to load localized media metadata");
            None
        }
    };

    let (posters, backdrops, poster_url, backdrop_url, related) =
        fetch_images_and_related(catalog, media_kind, &tmdb_id);
    let library_links = find_library_links(&state.store.lock(), &media);
    let cast = fetch_cast(&meta);
    let released = media.year.map(|y| format!("{y}-01-01")).unwrap_or_default();
    let seasons = fetch_tv_seasons(catalog, media_kind, &tmdb_id);
    let meta = meta.unwrap_or_default();

    ok(json!({
        "item": detail_item_json(&media, poster_url, backdrop_url),
        "info": {
            "overview": meta.overview,
            "rating": meta.rating,
            "runtime": meta.runtime_minutes,
            "genres": meta.genres,
            "cast": cast,
            "directors": [],
            "director_credits": [],
            "country": "",
            "language": "",
            "released": released,
            "aliases": [],
        },
        "seasons": seasons,
        "videos": [],
        "backdrops": backdrops,
        "posters": posters,
        "backdrop_original_url": backdrops.first().and_then(|b| b.get("full_url")).and_then(|v| v.as_str()).map(str::to_string),
        "related": related,
        "library_links": library_links,
    }))
    .into_response()
}

/// 200 空详情：条目在 TMDB 不存在（可能豆瓣 id 误进 TMDB 路由），前端
/// 用 seed（点卡片时的标题/海报）渲染；无 seed 时显示友好兜底而非报错。
fn empty_detail(kind: &str, id: &str) -> Response {
    ok(json!({
        "item": {
            "id": id,
            "title": Value::Null,
            "year": Value::Null,
            "kind": kind,
            "tmdb_id": Value::Null,
            "poster_url": Value::Null,
            "backdrop_url": Value::Null,
            "rating": Value::Null,
        },
        "info": {
            "overview": Value::Null,
            "rating": Value::Null,
            "runtime": Value::Null,
            "genres": [],
            "cast": [],
            "directors": [],
            "director_credits": [],
            "country": "",
            "language": "",
            "released": "",
            "aliases": [],
        },
        "videos": [],
        "backdrops": [],
        "posters": [],
        "related": [],
        "library_links": [],
    }))
    .into_response()
}

/// Resolve the TMDB id for a stable reference. Accepts `tmdb:<kind>:<id>`,
/// a numeric TMDB id, or a Douban id (looked up via media aliases / catalog).
fn tmdb_id_for(
    state: &ApiState,
    catalog: &dyn Catalog,
    kind: domain::MediaKind,
    raw: &str,
) -> Result<Option<String>, Response> {
    let tid = raw
        .strip_prefix("tmdb:")
        .and_then(|rest| rest.split(':').next_back())
        .map(str::to_string)
        .unwrap_or_else(|| raw.to_string());
    if tid.chars().all(|c| c.is_ascii_digit()) && !tid.is_empty() {
        return Ok(Some(tid));
    }
    // Douban id: alias lookup in local media table, then catalog search.
    let store = state.store.lock();
    if let Some(media) = store.get_media_by_alias("douban_id", &tid).ok().flatten() {
        if let Some(tmdb) = media.tmdb_id {
            return Ok(Some(tmdb));
        }
    }
    drop(store);
    match kind {
        domain::MediaKind::Movie => catalog.search_movie(raw).map(|hits| {
            hits.into_iter()
                .find(|h| h.media.douban_id.as_deref() == Some(raw))
                .and_then(|h| h.media.tmdb_id)
        }),
        _ => catalog.search_tv(raw).map(|hits| {
            hits.into_iter()
                .find(|h| h.media.douban_id.as_deref() == Some(raw))
                .and_then(|h| h.media.tmdb_id)
        }),
    }
    .map_err(|error| err(StatusCode::BAD_GATEWAY, "media.upstream", &error))
}

fn item_json(media: &domain::Media, poster_path: Option<&str>) -> Value {
    json!({
        "id": media.tmdb_id.clone().unwrap_or_else(|| media.id.to_string()),
        "title": media.title,
        "year": media.year,
        "kind": media.kind.as_str(),
        "tmdb_id": media.tmdb_id,
        "poster_url": remote_or_prefixed(poster_path, TMDB_POSTER),
        "backdrop_url": Value::Null,
    })
}

fn detail_item_json(
    media: &domain::Media,
    poster_url: Option<String>,
    backdrop_url: Option<String>,
) -> Value {
    let mut item = item_json(media, None);
    item["poster_url"] = json!(poster_url);
    item["backdrop_url"] = json!(backdrop_url);
    item
}

fn tmdb_image_url_at_size(url: &str, size: &str) -> String {
    url.replace("/t/p/w342/", &format!("/t/p/{size}/"))
}

fn image_json(candidate: &crate::catalog::ArtworkCandidate) -> Value {
    json!({
        "preview_url": candidate.url("w780"),
        "full_url": candidate.url("original"),
        "width": candidate.width,
        "height": candidate.height,
    })
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

/// `GET /media/person/{id}?page=1&page_size=40` — TMDB person + combined
/// credits, paged: TMDB's combined_credits returns everything at once (no
/// server-side paging), so we slice here and cache the full response.
pub(crate) async fn person_detail(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(query): Query<PersonPageQuery>,
) -> Response {
    let state_clone = state.clone();
    let id_clone = id.clone();
    let person_handle = state
        .http_work
        .run(MEDIA_DEADLINE, move || {
            let catalog: &dyn Catalog = state_clone.catalog.as_ref();
            catalog.person_details(&id_clone)
        })
        .await;
    let details = match person_handle {
        Ok(Ok(Some(details))) => details,
        Ok(Ok(None)) => return err(StatusCode::NOT_FOUND, "media.person_missing", "影人不存在"),
        Ok(Err(error)) => return err(StatusCode::BAD_GATEWAY, "media.upstream", &error),
        Err(error) => return blocking_error(error, "media.timeout"),
    };
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(40).clamp(10, 100);
    let total = details.items.len() as i64;
    let start = ((page - 1) * page_size) as usize;
    let slice = details
        .items
        .into_iter()
        .skip(start)
        .take(page_size as usize);
    let items: Vec<Value> = slice
        .map(|credit| {
            json!({
                "id": credit.tmdb_id,
                "title": credit.title,
                "year": credit.year,
                "kind": credit.kind.as_str(),
                "tmdb_id": credit.tmdb_id.to_string(),
                "poster_url": credit.poster_path.as_deref().map(|p| format!("https://image.tmdb.org/t/p/w342{p}")),
                "rating": Value::Null,
            })
        })
        .collect();
    let total_pages = (total + page_size - 1) / page_size;
    ok(json!({
        "tmdb_person_id": id,
        "name": details.name,
        "avatar_url": details.profile_path.as_deref().map(|p| format!("https://image.tmdb.org/t/p/w185{p}")),
        "items": items,
        "page": page,
        "total_pages": total_pages,
        "total_results": total,
        "has_more": page < total_pages,
    }))
    .into_response()
}

#[derive(Deserialize, Default)]
pub(crate) struct PersonPageQuery {
    page: Option<i64>,
    page_size: Option<i64>,
}

/// `GET /media/douban/{id}` — douban subject id → TMDB detail (kind probed
/// via the catalog, since douban ids don't encode movie/tv).
pub(crate) async fn douban_detail(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let state_clone = state.clone();
    let id_clone = id.clone();
    let res = state
        .http_work
        .run(MEDIA_DEADLINE, move || {
            douban_detail_blocking(&state_clone, &id_clone)
        })
        .await;
    match res {
        Ok(resp) => resp,
        Err(error) => blocking_error(error, "media.timeout"),
    }
}

fn douban_detail_blocking(state: &ApiState, id: &str) -> Response {
    // 优先本地 media 表映射（订阅/认领时 douban_id → tmdb_id 已入库）：
    // 有映射走完整 TMDB 详情。
    let store = state.store.lock();
    let local_media = store.get_media_by_alias("douban_id", id).ok().flatten();
    drop(store);
    if let Some(media) = local_media {
        if let Some(tmdb_id) = media.tmdb_id {
            let kind = if media.kind == domain::MediaKind::Tv {
                "tv"
            } else {
                "movie"
            };
            return media_detail_inner(state, kind, &tmdb_id);
        }
    }

    // 豆瓣 Rexxar 移动 API：实时拉取该条目详情与演职员表（带头像）
    let (douban_rexxar, douban_celebrities) = {
        let rexxar = fetch_douban_rexxar(id);
        let celeb = fetch_douban_celebrities(id);
        (rexxar, celeb)
    };

    if let Some(data) = douban_rexxar {
        let is_tv = data.get("type").and_then(|v| v.as_str()) == Some("tv")
            || data.get("is_tv").and_then(|v| v.as_bool()).unwrap_or(false);
        let kind = if is_tv { "tv" } else { "movie" };
        let title = data
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let year = data
            .get("year")
            .and_then(|v| v.as_str())
            .and_then(|y| y.parse::<u16>().ok());
        let poster_url = data
            .get("cover_url")
            .and_then(|v| v.as_str())
            .or_else(|| {
                data.get("pic")
                    .and_then(|p| p.get("large"))
                    .and_then(|v| v.as_str())
            })
            .map(str::to_string);
        let rating = data
            .get("rating")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_f64());
        let overview = data
            .get("intro")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let genres: Vec<Value> = data
            .get("genres")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // 演员头像优先使用 /celebrities 接口
        let cast: Vec<Value> = if let Some(celeb_actors) = douban_celebrities
            .as_ref()
            .and_then(|c| c.get("actors"))
            .and_then(|v| v.as_array())
        {
            celeb_actors.iter().take(16).map(|a| {
                json!({
                    "name": a.get("name").and_then(|v| v.as_str()).unwrap_or_default(),
                    "role": a.get("character").and_then(|v| v.as_str()),
                    "tmdb_person_id": Value::Null,
                    "avatar_url": a.get("avatar").and_then(|av| av.get("large").or_else(|| av.get("normal"))).and_then(|v| v.as_str()),
                })
            }).collect()
        } else {
            data.get("actors").and_then(|v| v.as_array()).map(|arr| {
                arr.iter().take(10).map(|a| {
                    json!({
                        "name": a.get("name").and_then(|v| v.as_str()).unwrap_or_default(),
                        "role": a.get("character").and_then(|v| v.as_str()),
                        "tmdb_person_id": Value::Null,
                        "avatar_url": a.get("avatar").and_then(|av| av.get("large").or_else(|| av.get("normal"))).and_then(|v| v.as_str()),
                    })
                }).collect()
            }).unwrap_or_default()
        };

        let directors: Vec<Value> = data
            .get("directors")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|d| d.get("name")).cloned().collect())
            .unwrap_or_default();
        let countries = data
            .get("countries")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| c.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            })
            .unwrap_or_default();
        let languages = data
            .get("languages")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|l| l.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            })
            .unwrap_or_default();
        let released = data
            .get("release_date")
            .and_then(|v| v.as_str())
            .or_else(|| {
                data.get("pubdate")
                    .and_then(|p| p.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|v| v.as_str())
            })
            .unwrap_or_default();
        let aliases: Vec<Value> = data
            .get("aka")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let posters: Vec<Value> = poster_url
            .as_deref()
            .map(|p| {
                vec![json!({
                    "preview_url": p,
                    "full_url": p,
                    "width": 300,
                    "height": 450,
                })]
            })
            .unwrap_or_default();

        let backdrops: Vec<Value> = poster_url
            .as_deref()
            .map(|p| {
                vec![json!({
                    "preview_url": p,
                    "full_url": p,
                    "width": 300,
                    "height": 450,
                })]
            })
            .unwrap_or_default();

        return ok(json!({
            "item": {
                "id": id,
                "title": title,
                "year": year,
                "kind": kind,
                "tmdb_id": Value::Null,
                "poster_url": poster_url.clone(),
                "backdrop_url": poster_url.clone(),
                "rating": rating,
            },
            "info": {
                "overview": overview,
                "rating": rating.map(|r| format!("{r:.1}")),
                "runtime": Value::Null,
                "genres": genres,
                "cast": cast,
                "directors": directors,
                "director_credits": [],
                "country": countries,
                "language": languages,
                "released": released,
                "aliases": aliases,
            },
            "videos": [],
            "backdrops": backdrops,
            "posters": posters,
            "related": [],
            "library_links": [],
        }))
        .into_response();
    }

    ok(json!({
        "item": {
            "id": id,
            "title": Value::Null,
            "year": Value::Null,
            "kind": "movie",
            "tmdb_id": Value::Null,
            "poster_url": Value::Null,
            "backdrop_url": Value::Null,
            "rating": Value::Null,
        },
        "info": {
            "overview": Value::Null,
            "rating": Value::Null,
            "runtime": Value::Null,
            "genres": [],
            "cast": [],
            "directors": [],
            "director_credits": [],
            "country": "",
            "language": "",
            "released": "",
            "aliases": [],
        },
        "videos": [],
        "backdrops": [],
        "posters": [],
        "related": [],
        "library_links": [],
    }))
    .into_response()
}

fn fetch_douban_rexxar(id: &str) -> Option<Value> {
    const UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15";
    let url = format!("https://m.douban.com/rexxar/api/v2/movie/{id}?for_mobile=1");
    let response = crate::http_agent::call_douban(|agent| {
        agent
            .get(&url)
            .header("User-Agent", UA)
            .header("Referer", "https://m.douban.com/movie/")
            .call()
    })
    .ok()?;
    let body = response.into_body().read_to_string().ok()?;
    serde_json::from_str(&body).ok()
}

fn fetch_douban_celebrities(id: &str) -> Option<Value> {
    const UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15";
    let url = format!("https://m.douban.com/rexxar/api/v2/movie/{id}/celebrities?for_mobile=1");
    let response = crate::http_agent::call_douban(|agent| {
        agent
            .get(&url)
            .header("User-Agent", UA)
            .header("Referer", "https://m.douban.com/movie/")
            .call()
    })
    .ok()?;
    let body = response.into_body().read_to_string().ok()?;
    serde_json::from_str(&body).ok()
}
