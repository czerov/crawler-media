//! Search: cross-site torrent search (JSON + SSE), catalog title search,
//! library search, history, and presets.

use std::collections::HashMap;
use std::convert::Infallible;
use std::time::Instant;

use axum::extract::{Query, RawQuery, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use domain::Torrent;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::http::{err, ok, ok_list};
use crate::management::ApiState;

fn release_json(torrent: &Torrent) -> Value {
    let release = release::parse(&torrent.title);
    json!({
        "title": release.title,
        "year": release.year,
        "season": release.season,
        "episode": release.episode,
        "episode_to": release.episode_to,
        "resolution": release.resolution,
        "source": release.source,
        "codec": release.codec,
        "hdr": release.hdr,
        "group": release.group,
        "confidence": release.confidence.as_str(),
    })
}

fn torrent_json(torrent: &Torrent, site_name: &str) -> Value {
    json!({
        "site_id": torrent.site_id.to_string(),
        "site_name": site_name,
        "id": torrent.id,
        "title": torrent.title,
        "enclosure": torrent.enclosure,
        "size_bytes": torrent.size_bytes,
        "seeders": torrent.seeders,
        "leechers": torrent.leechers,
        "snatched": torrent.snatched,
        "upload_time": torrent.upload_time,
        "poster_url": torrent.poster_url,
        "free": torrent.free,
        "hr": torrent.hr,
        "imdb_id": torrent.imdb_id,
        "detail_url": torrent.detail_url,
        "category": torrent.category,
        "release": release_json(torrent),
    })
}

#[derive(Deserialize)]
pub(crate) struct TorrentSearchParams {
    keyword: Option<String>,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    save_history: bool,
    #[serde(default)]
    skip_history: bool,
}

fn query_values(raw: Option<&str>, key: &str) -> Vec<String> {
    raw.into_iter()
        .flat_map(|query| url::form_urlencoded::parse(query.as_bytes()))
        .filter(|(name, _)| name == key)
        .flat_map(|(_, value)| value.split(',').map(str::to_string).collect::<Vec<_>>())
        .filter(|value| !value.is_empty())
        .collect()
}

/// Enabled sites, narrowed to the requested `sites` when the caller passed any.
fn scoped_sites(store: &crate::Store, requested: &[String]) -> Vec<domain::Site> {
    let all = store.list_enabled_sites().unwrap_or_default();
    if requested.is_empty() {
        return all;
    }
    all.into_iter()
        .filter(|site| requested.iter().any(|raw| raw == &site.id.to_string()))
        .collect()
}

pub(crate) async fn search_torrents(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Query(query): Query<TorrentSearchParams>,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let sites_requested = query_values(raw_query.as_deref(), "sites");
    let categories = query_values(raw_query.as_deref(), "categories");
    let keyword = query
        .keyword
        .map(|k| k.trim().to_string())
        .unwrap_or_default();
    let page = query.page.unwrap_or(1).clamp(1, 100);
    let history = (!keyword.is_empty() && query.save_history && !query.skip_history && page == 1)
        .then(|| history::next_history_target(user_id));
    let sites = {
        let store = state.store.lock();
        scoped_sites(&store, &sites_requested)
    };
    // Site fetches are blocking HTTP; run them under the shared search budget in a
    // window of the currently available permits, mirroring the SSE path. Spawning
    // every Site at once used to make Sites beyond the 4 permits fail with
    // "capacity exhausted" inside the very same request.
    let state = Arc::new(state);
    let (total, items, statuses) =
        fanout::collect_site_results(&state, sites, &keyword, page, &categories, None).await;
    history::save_torrents_history(
        &state,
        history.as_ref(),
        &keyword,
        total,
        None,
        &items,
        &statuses,
        false,
    );
    ok(json!({
        "keyword": keyword,
        "total": total,
        "items": items,
        "sites": statuses,
    }))
    .into_response()
}

pub(crate) async fn search_stream(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Query(query): Query<TorrentSearchParams>,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let sites_requested = query_values(raw_query.as_deref(), "sites");
    let categories = query_values(raw_query.as_deref(), "categories");
    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(16);
    let keyword = query.keyword.clone().unwrap_or_default().trim().to_string();
    let page = query.page.unwrap_or(1).clamp(1, 100);
    let history = (!keyword.is_empty() && query.save_history && !query.skip_history && page == 1)
        .then(|| history::next_history_target(user_id));
    let sites = {
        let store = state.store.lock();
        scoped_sites(&store, &sites_requested)
    };
    tokio::spawn(stream_search_events(
        Arc::new(state),
        tx,
        keyword,
        page,
        history,
        sites,
        categories,
    ));
    Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn stream_search_events(
    state: Arc<ApiState>,
    tx: mpsc::Sender<Result<Event, Infallible>>,
    keyword: String,
    page: u32,
    history: Option<history::HistoryTarget>,
    sites: Vec<domain::Site>,
    categories: Vec<String>,
) {
    send_search_start(&tx, &keyword, &sites).await;
    let started = Instant::now();
    let (total, items, statuses) =
        fanout::collect_site_results(&state, sites, &keyword, page, &categories, Some(&tx)).await;
    let elapsed_ms = started.elapsed().as_millis() as i64;
    // 客户端断开也要落盘已收集结果：历史条目与快照必须成对，否则回放只能拿到
    // 404 死链（历史里有条目 ⇒ 快照一定可取）。
    let partial = tx.is_closed();
    history::save_torrents_history(
        &state,
        history.as_ref(),
        &keyword,
        total,
        Some(elapsed_ms),
        &items,
        &statuses,
        partial,
    );
    if partial {
        return;
    }
    send_search_done(&tx, total, elapsed_ms, statuses).await;
}

async fn send_search_start(
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    keyword: &str,
    sites: &[domain::Site],
) {
    let site_list: Vec<Value> = sites
        .iter()
        .map(|site| json!({"site_id": site.id.to_string(), "site_name": site.name}))
        .collect();
    let _ = tx
        .send(Ok(Event::default().event("start").data(
            json!({"keyword": keyword, "sites": site_list}).to_string(),
        )))
        .await;
}

async fn send_search_done(
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    total: i64,
    elapsed_ms: i64,
    statuses: Vec<Value>,
) {
    let _ = tx
        .send(Ok(Event::default().event("done").data(
            json!({"total": total, "elapsed_ms": elapsed_ms, "sites": statuses}).to_string(),
        )))
        .await;
}

use media::CatalogHit;

fn title_hit_json(hit: &CatalogHit, provider: &str) -> Value {
    let media = &hit.media;
    let external_id = media
        .tmdb_id
        .clone()
        .or_else(|| media.douban_id.clone())
        .or_else(|| media.tvdb_id.clone())
        .or_else(|| media.bangumi_id.clone())
        .or_else(|| media.anilist_id.clone())
        .unwrap_or_default();
    let poster = hit.poster_path.as_ref().map(|p: &String| {
        if p.starts_with("http://") || p.starts_with("https://") {
            p.clone()
        } else {
            format!("https://image.tmdb.org/t/p/w342{p}")
        }
    });
    json!({
        "provider": provider,
        "external_id": external_id,
        "kind": media.kind.as_str(),
        "title": media.title,
        "year": media.year,
        "original_title": media.original_title,
        "poster_url": poster,
    })
}

/// 单个元数据源的 blocking 搜索：返回 (hits, ok, count, error)。
fn search_catalog_source(
    catalog: &dyn crate::catalog::Catalog,
    source: &str,
    keyword: &str,
) -> (Vec<CatalogHit>, bool, i64, Option<String>) {
    let Some(source_catalog) = catalog.source(source) else {
        return (
            Vec::new(),
            false,
            0,
            Some("未配置或不存在该数据源".to_string()),
        );
    };
    let movies = source_catalog.search_movie(keyword);
    let shows = source_catalog.search_tv(keyword);
    let mut hits = Vec::new();
    let mut ok = true;
    let mut err_msg = None;
    for result in [movies, shows] {
        match result {
            Ok(found) => hits.extend(found),
            Err(error) => {
                ok = false;
                err_msg = Some(error);
            }
        }
    }
    let count = hits.len() as i64;
    (hits, ok, count, err_msg)
}


pub(crate) async fn search_titles(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let keyword = query.get("keyword").cloned().unwrap_or_default();
    if keyword.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "search.query", "keyword 必填");
    }
    let provider = query
        .get("provider")
        .cloned()
        .unwrap_or_else(|| "all".into());
    // provider 定向：tmdb/douban/tvdb/bangumi/anilist 单源；缺省 fanout 全源。
    let catalog: &dyn crate::catalog::Catalog = state.catalog.as_ref();
    let targeted = provider != "all" && !provider.is_empty();
    let sources: Vec<String> = if targeted {
        match catalog.source(&provider) {
            Some(_) => vec![provider.clone()],
            None => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "search.provider",
                    &format!("未知的 provider {provider}"),
                );
            }
        }
    } else {
        catalog.sources().into_iter().map(str::to_string).collect()
    };
    let history_target = history::next_history_target(user_id);
    // Fanout 各元数据源：与 Site 搜索共用「可用 permit 开窗 + 请求级 deadline」
    // 策略，所以默认 5 个源不会再恒定有一个被本请求自己的并发预算判成过载。
    let state = Arc::new(state);
    let (titles, providers) = fanout::search_title_sources(&state, sources, &keyword).await;
    history::save_titles_history(
        &state,
        Some(&history_target),
        &keyword,
        &provider,
        &titles,
        &providers,
    );
    ok(json!({
        "query": keyword,
        "provider": provider,
        "titles": titles,
        "providers": providers,
    }))
    .into_response()
}

pub(crate) async fn search_library_items(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let keyword = query.get("keyword").cloned().unwrap_or_default();
    let res = state
        .search_work
        .run(fanout::SEARCH_DEADLINE, move || {
            let store = state.store.lock();
            let rows = store.list_ledger().unwrap_or_default();
            let mut items = Vec::new();
            for row in rows {
                let Some(media) = store.get_media(row.media_id).ok().flatten() else {
                    continue;
                };
                if !crate::http::library::row_visible_to_user(&store, &row, &media, user_id) {
                    continue;
                }
                if !keyword.is_empty() && !media.title.to_lowercase().contains(&keyword.to_lowercase()) {
                    continue;
                }
                items.push(json!({
                    "media_item_id": media.id.to_string(),
                    "title": media.title,
                    "kind": media.kind.as_str(),
                    "path": row.path,
                    "season": row.season,
                    "episode": row.episode,
                }));
            }
            items
        })
        .await;

    match res {
        Ok(items) => ok_list(items).into_response(),
        Err(error) => {
            tracing::warn!(%error, "库内搜索未获搜索预算准入");
            if matches!(error, crate::blocking_budget::BlockingError::Busy) {
                err(StatusCode::SERVICE_UNAVAILABLE, "search.busy", "搜索并发过载")
            } else {
                // 用户可见文案必须是可读中文；具体英文错误只进日志。
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "search.failed",
                    "搜索执行失败，请稍后重试",
                )
            }
        }
    }
}

mod fanout;
mod history;
pub(crate) use history::{
    clear_search_history_route, delete_history_entry_route, get_history_snapshot, get_presets,
    put_presets, search_history,
};
