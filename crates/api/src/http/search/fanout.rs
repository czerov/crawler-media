//! 搜索 fanout 的有界并发窗口（B3 §6）。
//!
//! 每次搜索请求在**当前可用 permit** 的窗口内并发执行，某单元结束（或请求级
//! deadline 已过）后再补下一个。JSON 与 SSE 共用同一套窗口策略，所以“一次请求
//! 里站点/数据源数超过 4”不会再让后面的单元自己把自己判成过载饿死。
//!
//! 预算类失败一律映射成可读中文原因（web 契约：单站搜索失败给中文原因），
//! 不把 `blocking_budget` 的英文错误直接透传给用户。

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::response::sse::Event;
use indexer::{SearchOutcome, SiteFailure};
use media::CatalogHit;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::blocking_budget::BlockingError;
use crate::management::ApiState;

use super::{search_catalog_source, title_hit_json, torrent_json};

/// B3 §6：搜索总准入 4、请求 deadline 12 秒。
pub(super) const SEARCH_DEADLINE: Duration = Duration::from_secs(12);

/// 预算类失败的可读中文原因（用户可见文案，不外泄英文实现细节）。
pub(super) fn budget_failure_message(error: &BlockingError) -> String {
    match error {
        BlockingError::Busy => "搜索并发已满，请稍后重试".to_string(),
        BlockingError::Timeout => format!("数据源响应超时(>{}秒)", SEARCH_DEADLINE.as_secs()),
        BlockingError::Worker(detail) => format!("任务执行异常: {detail}"),
    }
}

fn site_status_json(
    site_id: &str,
    site_name: &str,
    count: usize,
    error: Option<String>,
    elapsed_ms: i64,
) -> Value {
    json!({
        "site_id": site_id,
        "site_name": site_name,
        "count": count,
        "error": error,
        "elapsed_ms": elapsed_ms,
    })
}

struct SiteSearchResult {
    site_id: String,
    site_name: String,
    outcome: SearchOutcome,
    elapsed_ms: i64,
}

/// 启动单个 Site 搜索：阻塞工作经搜索预算准入，失败降级为该 Site 的失败结果。
fn spawn_site_search(
    tasks: &mut tokio::task::JoinSet<SiteSearchResult>,
    state: &Arc<ApiState>,
    site: domain::Site,
    keyword: &str,
    page: u32,
    categories: &[String],
    budget: Duration,
) {
    let pool = Arc::clone(&state.search_work);
    let indexer = Arc::clone(&state.indexer);
    let keyword = keyword.to_string();
    let categories = categories.to_vec();
    let site_id = site.id.to_string();
    let site_name = site.name.clone();
    let site_for_search = site.clone();
    tasks.spawn(async move {
        let started = Instant::now();
        let outcome = pool
            .run(budget, move || {
                indexer.search_page_categories(
                    std::slice::from_ref(&site_for_search),
                    &keyword,
                    page,
                    &categories,
                )
            })
            .await;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::warn!(
                    site_id = %site_id,
                    site = %site_name,
                    %error,
                    "站点搜索未获搜索预算，按单站失败报告"
                );
                SearchOutcome {
                    torrents: Vec::new(),
                    failures: vec![SiteFailure {
                        site_id: site.id,
                        error: budget_failure_message(&error),
                    }],
                }
            }
        };
        SiteSearchResult {
            site_id,
            site_name,
            outcome,
            elapsed_ms: started.elapsed().as_millis() as i64,
        }
    });
}

/// 按当前可用 permit 开窗并发搜索各 Site；请求级 deadline 内逐个补位。
///
/// - `tx = Some(..)`：SSE 模式，逐站推送事件，客户端断开即停止排新站。
/// - `tx = None`：JSON 模式，一次性收集；deadline 内没排上的站点如实报告为单站
///   失败，绝不静默丢弃（B3 §6「单Site失败独立报告」）。
pub(super) async fn collect_site_results(
    state: &Arc<ApiState>,
    sites: Vec<domain::Site>,
    keyword: &str,
    page: u32,
    categories: &[String],
    tx: Option<&mpsc::Sender<Result<Event, Infallible>>>,
) -> (i64, Vec<Value>, Vec<Value>) {
    let mut tasks = tokio::task::JoinSet::new();
    let mut pending = sites.into_iter();
    let window = state.search_work.available_permits().max(1);
    let deadline = Instant::now() + SEARCH_DEADLINE;
    let mut statuses = Vec::new();
    let mut items_all = Vec::new();
    let mut total = 0i64;
    loop {
        if tx.is_some_and(|tx| tx.is_closed()) {
            break;
        }
        while tasks.len() < window {
            let Some(site) = pending.next() else { break };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                // 请求预算已耗尽：剩余站点如实报为单站超时，仍出现在 sites 报告里。
                statuses.push(site_status_json(
                    &site.id.to_string(),
                    &site.name,
                    0,
                    Some(budget_failure_message(&BlockingError::Timeout)),
                    0,
                ));
                continue;
            }
            spawn_site_search(&mut tasks, state, site, keyword, page, categories, remaining);
        }
        let Some(result) = tasks.join_next().await else {
            break;
        };
        let Ok(result) = result else { continue };
        let SiteSearchResult {
            site_id,
            site_name,
            outcome,
            elapsed_ms,
        } = result;
        total += outcome.torrents.len() as i64;
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
        if let Some(tx) = tx {
            let event = if error.is_none() {
                "site_result"
            } else {
                "site_error"
            };
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
        }
        statuses.push(site_status_json(
            &site_id,
            &site_name,
            outcome.torrents.len(),
            error,
            elapsed_ms,
        ));
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

fn provider_error_json(source: &str, message: &str) -> Value {
    json!({
        "provider": source,
        "ok": false,
        "count": 0,
        "message": message,
    })
}

type TitleSourceOutcome =
    Result<(String, Vec<CatalogHit>, bool, i64, Option<String>), (String, String)>;

fn spawn_title_source(
    set: &mut tokio::task::JoinSet<TitleSourceOutcome>,
    state: &Arc<ApiState>,
    source: String,
    keyword: &str,
    budget: Duration,
) {
    let pool = Arc::clone(&state.search_work);
    let catalog = Arc::clone(&state.catalog);
    let keyword = keyword.to_string();
    set.spawn(async move {
        let searched = source.clone();
        match pool
            .run(budget, move || {
                search_catalog_source(catalog.as_ref(), &searched, &keyword)
            })
            .await
        {
            Ok((hits, ok, count, err_msg)) => Ok((source, hits, ok, count, err_msg)),
            Err(error) => {
                tracing::warn!(
                    provider = %source,
                    %error,
                    "标题搜索数据源未获搜索预算，按单源失败报告"
                );
                Err((source, budget_failure_message(&error)))
            }
        }
    });
}

fn push_title_result(
    titles: &mut Vec<Value>,
    providers: &mut Vec<Value>,
    result: Result<TitleSourceOutcome, tokio::task::JoinError>,
) {
    match result {
        Ok(Ok((source, hits, ok, count, err_msg))) => {
            titles.extend(hits.iter().map(|hit| title_hit_json(hit, &source)));
            if ok {
                providers.push(json!({
                    "provider": source,
                    "ok": true,
                    "count": count,
                }));
            } else {
                providers.push(json!({
                    "provider": source,
                    "ok": false,
                    "count": count,
                    "message": err_msg.unwrap_or_else(|| "搜索失败".into()),
                }));
            }
        }
        Ok(Err((source, message))) => providers.push(provider_error_json(&source, &message)),
        Err(join_err) => providers.push(json!({
            "provider": "unknown",
            "ok": false,
            "count": 0,
            "message": format!("调度异常: {join_err}"),
        })),
    }
}

/// 逐源 fanout：与 Site 搜索共用「可用 permit 开窗 + 请求级 deadline 补位」策略，
/// 因此默认 5 个元数据源不会再恒定有一个被自己的并发预算判成过载。
pub(super) async fn search_title_sources(
    state: &Arc<ApiState>,
    sources: Vec<String>,
    keyword: &str,
) -> (Vec<Value>, Vec<Value>) {
    let mut set: tokio::task::JoinSet<TitleSourceOutcome> = tokio::task::JoinSet::new();
    let mut pending = sources.into_iter();
    let window = state.search_work.available_permits().max(1);
    let deadline = Instant::now() + SEARCH_DEADLINE;
    let mut titles = Vec::new();
    let mut providers = Vec::new();
    loop {
        while set.len() < window {
            let Some(source) = pending.next() else { break };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                // 请求预算已耗尽：剩余数据源如实报失败，仍出现在 providers 报告里。
                providers.push(provider_error_json(
                    &source,
                    &budget_failure_message(&BlockingError::Timeout),
                ));
                continue;
            }
            spawn_title_source(&mut set, state, source, keyword, remaining);
        }
        let Some(result) = set.join_next().await else {
            break;
        };
        push_title_result(&mut titles, &mut providers, result);
    }
    (titles, providers)
}
