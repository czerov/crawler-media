use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::management::{ApiError, ApiState};
use crate::store::CatalogCacheRow;

#[derive(serde::Serialize, Clone)]
pub struct MediaCacheEntry {
    pub id: String,
    pub source: String,
    pub source_id: String,
    pub title: String,
    pub original_title: Option<String>,
    pub kind: String, // "movie" | "tv"
    pub year: Option<u32>,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    pub overview: Option<String>,
    pub rating: Option<f64>,
    pub genres: Vec<String>,
    pub fetched_at: i64,
    pub expires_at: Option<i64>,
    pub cached_keys: Vec<String>,
}

#[derive(Deserialize)]
pub struct ListCatalogQuery {
    pub view: Option<String>,
}

pub async fn list_catalog_cache(
    State(state): State<ApiState>,
    Query(query): Query<ListCatalogQuery>,
) -> Result<Json<Value>, ApiError> {
    let rows = state.store.lock().list_catalog_cache()?;
    if query.view.as_deref() == Some("aggregate") {
        let items = aggregate_media_entries(rows);
        let list_val: Vec<Value> = items
            .into_iter()
            .map(|item| {
                json!({
                    "id": item.id,
                    "source": item.source,
                    "source_id": item.source_id,
                    "title": item.title,
                    "original_title": item.original_title,
                    "kind": item.kind,
                    "year": item.year,
                    "poster_url": item.poster_url,
                    "backdrop_url": item.backdrop_url,
                    "overview": item.overview,
                    "rating": item.rating,
                    "genres": item.genres,
                    "fetched_at": item.fetched_at,
                    "expires_at": item.expires_at,
                    "cached_keys": item.cached_keys,
                })
            })
            .collect();

        return Ok(Json(json!({
            "ok": true,
            "data": list_val,
        })));
    }

    let raw_val: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            let val: Option<Value> = serde_json::from_str(row.body.trim()).ok();
            let title = val
                .as_ref()
                .and_then(|v| {
                    v.get("title")
                        .or_else(|| v.get("name"))
                        .and_then(Value::as_str)
                        .or_else(|| v.pointer("/results/0/title").and_then(Value::as_str))
                        .or_else(|| v.pointer("/results/0/name").and_then(Value::as_str))
                })
                .unwrap_or("-");
            json!({
                "source": row.source,
                "cache_key": row.cache_key,
                "fetched_at": row.fetched_at,
                "expires_at": row.expires_at,
                "title": title,
            })
        })
        .collect();
    Ok(Json(json!({
        "ok": true,
        "data": raw_val,
    })))
}

#[derive(Deserialize)]
pub struct DeleteMediaCacheQuery {
    source: String,
    cache_key: Option<String>,
}

pub async fn delete_catalog_cache(
    State(state): State<ApiState>,
    Query(query): Query<DeleteMediaCacheQuery>,
) -> Result<Json<Value>, ApiError> {
    let key = query
        .cache_key
        .filter(|key| !key.is_empty())
        .ok_or_else(|| ApiError::invalid("catalog.invalid", "cache_key is required".into()))?;
    let deleted = {
        let store = state.store.lock();
        store.delete_catalog_cache(&query.source, &key)?
    };
    if deleted {
        // catalog body 影响所有 Library 的补充字段：保守失效全部已访问投影。
        state.projection.mark_all_dirty();
    }
    Ok(Json(json!({ "ok": true, "data": { "deleted": deleted } })))
}

fn parse_year(date_str: Option<&str>) -> Option<u32> {
    let s = date_str?;
    if s.len() >= 4 {
        s[0..4].parse::<u32>().ok()
    } else {
        None
    }
}

/// 将零散的 HTTP 缓存按【影视实体】进行聚合
fn aggregate_media_entries(rows: Vec<CatalogCacheRow>) -> Vec<MediaCacheEntry> {
    let mut map: HashMap<String, MediaCacheEntry> = HashMap::new();

    for row in rows {
        let body_trimmed = row.body.trim();
        // 忽略纯 HTML（拦截页面）
        if body_trimmed.starts_with("<!DOCTYPE") || body_trimmed.starts_with("<html") {
            continue;
        }
        let Ok(val) = serde_json::from_str::<Value>(body_trimmed) else {
            continue;
        };

        // 1. TMDB 实体缓存 (/movie/{id} 或 /tv/{id})
        if row.source == "tmdb" {
            let is_movie = row.cache_key.starts_with("/movie/");
            let is_tv = row.cache_key.starts_with("/tv/");
            if (is_movie || is_tv)
                && !row.cache_key.contains("/now_playing")
                && !row.cache_key.contains("/popular")
                && !row.cache_key.contains("/upcoming")
                && !row.cache_key.contains("/top_rated")
                && !row.cache_key.contains("/airing_today")
                && !row.cache_key.contains("/on_the_air")
            {
                let parts: Vec<&str> = row
                    .cache_key
                    .split('?')
                    .next()
                    .unwrap_or("")
                    .split('/')
                    .collect();
                if parts.len() >= 3 {
                    let source_id = parts[2].to_string();
                    if source_id.chars().all(|c| c.is_ascii_digit()) {
                        let kind = if is_movie { "movie" } else { "tv" };
                        let entry_key = format!("tmdb:{kind}:{source_id}");

                        let title = val
                            .get("title")
                            .or_else(|| val.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let original_title = val
                            .get("original_title")
                            .or_else(|| val.get("original_name"))
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        let year = parse_year(
                            val.get("release_date")
                                .or_else(|| val.get("first_air_date"))
                                .and_then(Value::as_str),
                        );
                        let poster = val
                            .get("poster_path")
                            .and_then(Value::as_str)
                            .map(|p| format!("https://image.tmdb.org/t/p/w500{p}"));
                        let backdrop = val
                            .get("backdrop_path")
                            .and_then(Value::as_str)
                            .map(|p| format!("https://image.tmdb.org/t/p/w1280{p}"));
                        let overview = val
                            .get("overview")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        let rating = val.get("vote_average").and_then(Value::as_f64);
                        let genres = val
                            .get("genres")
                            .and_then(Value::as_array)
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|g| {
                                        g.get("name").and_then(Value::as_str).map(str::to_string)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();

                        let entry = map.entry(entry_key).or_insert_with(|| MediaCacheEntry {
                            id: format!("tmdb_{kind}_{source_id}"),
                            source: "tmdb".into(),
                            source_id: source_id.clone(),
                            title: title.to_string(),
                            original_title,
                            kind: kind.to_string(),
                            year,
                            poster_url: poster.clone(),
                            backdrop_url: backdrop.clone(),
                            overview: overview.clone(),
                            rating,
                            genres,
                            fetched_at: row.fetched_at,
                            expires_at: row.expires_at,
                            cached_keys: Vec::new(),
                        });

                        if !title.is_empty()
                            && (entry.title.is_empty() || row.cache_key.contains("language=zh-CN"))
                        {
                            entry.title = title.to_string();
                        }
                        if entry.poster_url.is_none() && poster.is_some() {
                            entry.poster_url = poster;
                        }
                        if entry
                            .overview
                            .as_ref()
                            .map(|o| o.is_empty())
                            .unwrap_or(true)
                            && overview.is_some()
                        {
                            entry.overview = overview;
                        }
                        if entry.year.is_none() && year.is_some() {
                            entry.year = year;
                        }
                        if !entry.cached_keys.contains(&row.cache_key) {
                            entry.cached_keys.push(row.cache_key.clone());
                        }
                        continue;
                    }
                }
            }

            // TMDB 搜索或推荐列表中的条目也可以提取为影视条目
            let results = val.get("results").and_then(Value::as_array);
            if let Some(list) = results {
                for item in list {
                    if let Some(id) = item.get("id").and_then(Value::as_i64) {
                        let id_str = id.to_string();
                        let is_tv = item.get("first_air_date").is_some()
                            || item.get("name").is_some()
                            || row.cache_key.contains("/tv");
                        let kind = if is_tv { "tv" } else { "movie" };
                        let entry_key = format!("tmdb:{kind}:{id_str}");

                        let title = item
                            .get("title")
                            .or_else(|| item.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let original_title = item
                            .get("original_title")
                            .or_else(|| item.get("original_name"))
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        let year = parse_year(
                            item.get("release_date")
                                .or_else(|| item.get("first_air_date"))
                                .and_then(Value::as_str),
                        );
                        let poster = item
                            .get("poster_path")
                            .and_then(Value::as_str)
                            .map(|p| format!("https://image.tmdb.org/t/p/w500{p}"));
                        let backdrop = item
                            .get("backdrop_path")
                            .and_then(Value::as_str)
                            .map(|p| format!("https://image.tmdb.org/t/p/w1280{p}"));
                        let overview = item
                            .get("overview")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        let rating = item.get("vote_average").and_then(Value::as_f64);

                        let entry = map.entry(entry_key).or_insert_with(|| MediaCacheEntry {
                            id: format!("tmdb_{kind}_{id_str}"),
                            source: "tmdb".into(),
                            source_id: id_str,
                            title: title.to_string(),
                            original_title,
                            kind: kind.to_string(),
                            year,
                            poster_url: poster.clone(),
                            backdrop_url: backdrop.clone(),
                            overview: overview.clone(),
                            rating,
                            genres: Vec::new(),
                            fetched_at: row.fetched_at,
                            expires_at: row.expires_at,
                            cached_keys: Vec::new(),
                        });

                        if !title.is_empty()
                            && (entry.title.is_empty() || row.cache_key.contains("language=zh-CN"))
                        {
                            entry.title = title.to_string();
                        }
                        if entry.poster_url.is_none() && poster.is_some() {
                            entry.poster_url = poster;
                        }
                        if entry.year.is_none() && year.is_some() {
                            entry.year = year;
                        }
                        if !entry.cached_keys.contains(&row.cache_key) {
                            entry.cached_keys.push(row.cache_key.clone());
                        }
                    }
                }
                continue;
            }
        }

        // 2. 豆瓣实体缓存（如 search_subjects 列表中的条目，或单条 subject 缓存）
        if row.source == "douban" {
            let subjects = val.get("subjects").and_then(Value::as_array);
            if let Some(list) = subjects {
                for item in list {
                    if let Some(id) = item.get("id").and_then(Value::as_str) {
                        let title = item.get("title").and_then(Value::as_str).unwrap_or("");
                        let poster = item
                            .get("cover")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        let rate = item
                            .get("rate")
                            .and_then(Value::as_str)
                            .and_then(|r| r.parse::<f64>().ok());
                        let kind = if row.cache_key.contains("type=tv") {
                            "tv"
                        } else {
                            "movie"
                        };
                        let entry_key = format!("douban:{kind}:{id}");

                        let entry = map.entry(entry_key).or_insert_with(|| MediaCacheEntry {
                            id: format!("douban_{kind}_{id}"),
                            source: "douban".into(),
                            source_id: id.to_string(),
                            title: title.to_string(),
                            original_title: None,
                            kind: kind.to_string(),
                            year: None,
                            poster_url: poster.clone(),
                            backdrop_url: None,
                            overview: None,
                            rating: rate,
                            genres: Vec::new(),
                            fetched_at: row.fetched_at,
                            expires_at: row.expires_at,
                            cached_keys: Vec::new(),
                        });
                        if !entry.cached_keys.contains(&row.cache_key) {
                            entry.cached_keys.push(row.cache_key.clone());
                        }
                    }
                }
                continue;
            }
        }

        // 3. AniList 动漫番剧实体（从媒体列表中提取真正有结果的番剧）
        if row.source == "anilist" {
            let media_list = val.pointer("/data/Page/media").and_then(Value::as_array);
            if let Some(list) = media_list {
                for item in list {
                    if let Some(id) = item.get("id").and_then(Value::as_i64) {
                        let id_str = id.to_string();
                        let title = item
                            .pointer("/title/native")
                            .or_else(|| item.pointer("/title/romaji"))
                            .or_else(|| item.pointer("/title/english"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let poster = item
                            .pointer("/coverImage/large")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        let banner = item
                            .get("bannerImage")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        let overview = item
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        let format = item.get("format").and_then(Value::as_str).unwrap_or("TV");
                        let kind = if format == "MOVIE" { "movie" } else { "tv" };
                        let entry_key = format!("anilist:{kind}:{id_str}");

                        let entry = map.entry(entry_key).or_insert_with(|| MediaCacheEntry {
                            id: format!("anilist_{kind}_{id_str}"),
                            source: "anilist".into(),
                            source_id: id_str,
                            title: title.to_string(),
                            original_title: item
                                .pointer("/title/romaji")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            kind: kind.to_string(),
                            year: item
                                .pointer("/startDate/year")
                                .and_then(Value::as_i64)
                                .map(|y| y as u32),
                            poster_url: poster,
                            backdrop_url: banner,
                            overview,
                            rating: item
                                .get("averageScore")
                                .and_then(Value::as_f64)
                                .map(|s| s / 10.0),
                            genres: item
                                .get("genres")
                                .and_then(Value::as_array)
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|g| g.as_str().map(str::to_string))
                                        .collect()
                                })
                                .unwrap_or_default(),
                            fetched_at: row.fetched_at,
                            expires_at: row.expires_at,
                            cached_keys: Vec::new(),
                        });
                        if !entry.cached_keys.contains(&row.cache_key) {
                            entry.cached_keys.push(row.cache_key.clone());
                        }
                    }
                }
                continue;
            }
        }

        // 4. Bangumi 番组计划实体
        if row.source == "bangumi" {
            let list = val.get("list").and_then(Value::as_array);
            if let Some(items) = list {
                for item in items {
                    if let Some(id) = item.get("id").and_then(Value::as_i64) {
                        let id_str = id.to_string();
                        let title = item
                            .get("name_cn")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .or_else(|| item.get("name").and_then(Value::as_str))
                            .unwrap_or("");
                        let poster = item
                            .pointer("/images/large")
                            .or_else(|| item.pointer("/images/common"))
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        let year = parse_year(item.get("air_date").and_then(Value::as_str));
                        let overview = item
                            .get("summary")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        let rating = item.get("score").and_then(Value::as_f64);
                        let kind = if row.cache_key.contains("type=6") {
                            "movie"
                        } else {
                            "tv"
                        };
                        let entry_key = format!("bangumi:{kind}:{id_str}");

                        let entry = map.entry(entry_key).or_insert_with(|| MediaCacheEntry {
                            id: format!("bangumi_{kind}_{id_str}"),
                            source: "bangumi".into(),
                            source_id: id_str,
                            title: title.to_string(),
                            original_title: item
                                .get("name")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            kind: kind.to_string(),
                            year,
                            poster_url: poster,
                            backdrop_url: None,
                            overview,
                            rating,
                            genres: vec!["动画".into()],
                            fetched_at: row.fetched_at,
                            expires_at: row.expires_at,
                            cached_keys: Vec::new(),
                        });
                        if !entry.cached_keys.contains(&row.cache_key) {
                            entry.cached_keys.push(row.cache_key.clone());
                        }
                    }
                }
                continue;
            }
        }
    }

    let mut list: Vec<MediaCacheEntry> =
        map.into_values().filter(|m| !m.title.is_empty()).collect();
    // 按抓取时间倒序排列（最新缓存的在最前面）
    list.sort_by(|a, b| b.fetched_at.cmp(&a.fetched_at));
    list
}
