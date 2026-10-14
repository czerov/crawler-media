//! Search: cross-site torrent search (JSON + SSE), catalog title search,
//! library search, history, and presets.

use std::collections::HashMap;
use std::convert::Infallible;
use std::time::{Duration, Instant};

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
    let snapshot_id =
        if !keyword.is_empty() && query.save_history && !query.skip_history && page == 1 {
            let store = state.store.lock();
            record_history(&store, user_id, &keyword, "torrents")
        } else {
            None
        };
    let sites = {
        let store = state.store.lock();
        scoped_sites(&store, &sites_requested)
    };
    // Site fetches are blocking HTTP; run them off the async worker via
    // spawn_blocking. Cross-site parallelism lives inside indexer.search.
    // 并发执行各站点搜索：避免单站耗时或超时累加阻塞全流程，与流式搜索保持一致的并发性能表现
    let mut tasks = Vec::new();
    for site in sites {
        let indexer = state.indexer.clone();
        let kw = keyword.clone();
        let categories = categories.clone();
        tasks.push(tokio::task::spawn_blocking(move || {
            let started = Instant::now();
            let site_outcome =
                indexer.search_page_categories(std::slice::from_ref(&site), &kw, page, &categories);
            let status = json!({
                "site_id": site.id.to_string(),
                "site_name": site.name,
                "count": site_outcome.torrents.len(),
                "error": site_outcome.failures.into_iter().next().map(|f| f.error),
                "elapsed_ms": started.elapsed().as_millis() as i64,
            });
            let items: Vec<_> = site_outcome
                .torrents
                .iter()
                .map(|t| torrent_json(t, &site.name))
                .collect();
            (items, status)
        }));
    }

    let mut items = Vec::new();
    let mut statuses = Vec::new();
    for task in tasks {
        if let Ok((site_items, status)) = task.await {
            items.extend(site_items);
            statuses.push(status);
        }
    }
    if let Some(id) = &snapshot_id {
        let payload = json!({
            "keyword": keyword,
            "snapshot_at": chrono_like_now(),
            "total": items.len() as i64,
            "elapsed_ms": null,
            "items": items.clone(),
            "sites": statuses.clone(),
        });
        let store = state.store.lock();
        let _ = store.insert_search_snapshot(
            id,
            "torrents",
            &keyword,
            &payload.to_string(),
            crate::job_loop::unix_now(),
        );
    }
    ok(json!({
        "keyword": keyword,
        "total": items.len() as i64,
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
    let snapshot_id =
        if !keyword.is_empty() && query.save_history && !query.skip_history && page == 1 {
            let store = state.store.lock();
            record_history(&store, user_id, &keyword, "torrents")
        } else {
            None
        };
    let sites = {
        let store = state.store.lock();
        scoped_sites(&store, &sites_requested)
    };
    tokio::spawn(stream_search_events(
        Arc::new(state),
        tx,
        keyword,
        page,
        snapshot_id,
        sites,
        categories,
    ));
    Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::default())
        .into_response()
}

struct SiteSearchResult {
    site_id: String,
    site_name: String,
    outcome: indexer::SearchOutcome,
    elapsed_ms: i64,
}

async fn stream_search_events(
    state: Arc<ApiState>,
    tx: mpsc::Sender<Result<Event, Infallible>>,
    keyword: String,
    page: u32,
    snapshot_id: Option<String>,
    sites: Vec<domain::Site>,
    categories: Vec<String>,
) {
    send_search_start(&tx, &keyword, &sites).await;
    let started = Instant::now();
    let mut tasks = spawn_site_searches(state.clone(), keyword.clone(), page, sites, categories);
    let (total, items, statuses) = collect_site_results(&mut tasks, &tx).await;
    let elapsed_ms = started.elapsed().as_millis() as i64;
    save_stream_snapshot(
        &state,
        snapshot_id.as_deref(),
        &keyword,
        total,
        elapsed_ms,
        &items,
        &statuses,
    );
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

fn spawn_site_searches(
    state: Arc<ApiState>,
    keyword: String,
    page: u32,
    sites: Vec<domain::Site>,
    categories: Vec<String>,
) -> tokio::task::JoinSet<SiteSearchResult> {
    let mut tasks = tokio::task::JoinSet::new();
    for site in sites {
        let state = Arc::clone(&state);
        let keyword = keyword.clone();
        let categories = categories.clone();
        let site_id = site.id.to_string();
        let site_name = site.name.clone();
        let site_for_search = site.clone();
        tasks.spawn(async move {
            let started = Instant::now();
            let outcome = tokio::task::spawn_blocking(move || {
                state.indexer.search_page_categories(
                    std::slice::from_ref(&site_for_search),
                    &keyword,
                    page,
                    &categories,
                )
            })
            .await
            .unwrap_or_else(|err| indexer::SearchOutcome {
                torrents: Vec::new(),
                failures: vec![indexer::SiteFailure {
                    site_id: site.id,
                    error: err.to_string(),
                }],
            });
            SiteSearchResult {
                site_id,
                site_name,
                outcome,
                elapsed_ms: started.elapsed().as_millis() as i64,
            }
        });
    }
    tasks
}

async fn collect_site_results(
    tasks: &mut tokio::task::JoinSet<SiteSearchResult>,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
) -> (i64, Vec<Value>, Vec<Value>) {
    let mut statuses = Vec::new();
    let mut items_all = Vec::new();
    let mut total = 0i64;
    while let Some(result) = tasks.join_next().await {
        let Ok(result) = result else { continue };
        let SiteSearchResult {
            site_id,
            site_name,
            outcome,
            elapsed_ms,
        } = result;
        total += outcome.torrents.len() as i64;
        let event = if outcome.failures.is_empty() {
            "site_result"
        } else {
            "site_error"
        };
        let error = outcome
            .failures
            .first()
            .map(|failure| failure.error.clone());
        let items = outcome
            .torrents
            .iter()
            .map(|t| torrent_json(t, &site_name))
            .collect::<Vec<_>>();
        items_all.extend(items.clone());
        send_site_result(
            tx,
            event,
            &site_id,
            &site_name,
            outcome.torrents.len(),
            error.clone(),
            elapsed_ms,
            items,
        )
        .await;
        statuses.push(json!({
            "site_id": site_id,
            "site_name": site_name,
            "count": outcome.torrents.len(),
            "elapsed_ms": elapsed_ms,
            "error": error,
        }));
    }
    (total, items_all, statuses)
}

#[allow(clippy::too_many_arguments)]
async fn send_site_result(
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    event: &str,
    site_id: &str,
    site_name: &str,
    count: usize,
    error: Option<String>,
    elapsed_ms: i64,
    items: Vec<Value>,
) {
    let _ = tx
        .send(Ok(Event::default().event(event).data(
            json!({
                "site_id": site_id,
                "site_name": site_name,
                "count": count,
                "error": error,
                "elapsed_ms": elapsed_ms,
                "items": items,
            })
            .to_string(),
        )))
        .await;
}

fn save_stream_snapshot(
    state: &ApiState,
    snapshot_id: Option<&str>,
    keyword: &str,
    total: i64,
    elapsed_ms: i64,
    items: &[Value],
    sites: &[Value],
) {
    let Some(id) = snapshot_id else { return };
    let payload = json!({
        "keyword": keyword,
        "snapshot_at": chrono_like_now(),
        "total": total,
        "elapsed_ms": elapsed_ms,
        "items": items,
        "sites": sites,
    });
    let store = state.store.lock();
    if let Err(error) = store.insert_search_snapshot(
        id,
        "torrents",
        keyword,
        &payload.to_string(),
        crate::job_loop::unix_now(),
    ) {
        tracing::error!(%error, history_id = %id, "保存流式搜索结果快照失败");
    }
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
    let snapshot_id = {
        let store = state.store.lock();
        record_history(&store, user_id, &keyword, "titles")
    };
    let mut titles = Vec::new();
    let mut providers = Vec::new();

    // 并发 fanout 搜索各元数据源：每个源独立放入 spawn_blocking 并在 6 秒内超时。
    // 单源失败或超时绝不阻塞其他正常源（如 TMDB、豆瓣通常在 1 秒内返回）。
    let mut set: tokio::task::JoinSet<
        Result<(String, Vec<CatalogHit>, bool, i64, Option<String>), (String, String)>,
    > = tokio::task::JoinSet::new();

    for source in sources {
        let state_clone = state.clone();
        let keyword_clone = keyword.clone();
        let source_name = source.clone();
        set.spawn(async move {
            let task_source = source_name.clone();
            match tokio::time::timeout(
                Duration::from_secs(6),
                tokio::task::spawn_blocking(move || {
                    let catalog = state_clone.catalog.as_ref();
                    let Some(source_catalog) = catalog.source(&task_source) else {
                        return (
                            task_source,
                            Vec::new(),
                            false,
                            0i64,
                            Some("未配置或不存在该数据源".to_string()),
                        );
                    };
                    let movies = source_catalog.search_movie(&keyword_clone);
                    let shows = source_catalog.search_tv(&keyword_clone);
                    let mut hits = Vec::new();
                    let mut ok = true;
                    let mut err_msg = None;
                    for result in [movies, shows] {
                        match result {
                            Ok(h) => hits.extend(h),
                            Err(e) => {
                                ok = false;
                                err_msg = Some(e);
                            }
                        }
                    }
                    let count = hits.len() as i64;
                    (task_source, hits, ok, count, err_msg)
                }),
            )
            .await
            {
                Ok(Ok(val)) => Ok(val),
                Ok(Err(panic_err)) => Err((source_name, format!("任务执行异常: {panic_err}"))),
                Err(_) => Err((source_name, "数据源响应超时(>6s)".to_string())),
            }
        });
    }

    while let Some(res) = set.join_next().await {
        match res {
            Ok(Ok((source, hits, ok, count, err_msg))) => {
                if ok {
                    titles.extend(hits.iter().map(|hit| title_hit_json(hit, &source)));
                    providers.push(json!({
                        "provider": source,
                        "ok": true,
                        "count": count,
                    }));
                } else {
                    titles.extend(hits.iter().map(|hit| title_hit_json(hit, &source)));
                    providers.push(json!({
                        "provider": source,
                        "ok": false,
                        "count": count,
                        "message": err_msg.unwrap_or_else(|| "搜索失败".into()),
                    }));
                }
            }
            Ok(Err((source, err_msg))) => {
                providers.push(json!({
                    "provider": source,
                    "ok": false,
                    "count": 0,
                    "message": err_msg,
                }));
            }
            Err(join_err) => {
                providers.push(json!({
                    "provider": "unknown",
                    "ok": false,
                    "count": 0,
                    "message": format!("调度异常: {join_err}"),
                }));
            }
        }
    }
    if let Some(id) = &snapshot_id {
        let payload = json!({
            "query": keyword,
            "provider": provider,
            "snapshot_at": chrono_like_now(),
            "total": titles.len() as i64,
            "titles": titles.clone(),
            "providers": providers.clone(),
        });
        let store = state.store.lock();
        let _ = store.insert_search_snapshot(
            id,
            "titles",
            &keyword,
            &payload.to_string(),
            crate::job_loop::unix_now(),
        );
    }
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
        .run(std::time::Duration::from_secs(10), move || {
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
        Err(crate::blocking_budget::BlockingError::Busy) => {
            err(StatusCode::SERVICE_UNAVAILABLE, "search.busy", "搜索并发过载")
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, "search.failed", &e.to_string()),
    }
}

mod history;
use history::{chrono_like_now, record_history};
pub(crate) use history::{
    clear_search_history_route, delete_history_entry_route, get_history_snapshot, get_presets,
    put_presets, search_history,
};
