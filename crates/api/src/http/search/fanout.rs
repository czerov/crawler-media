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

/// 任务/worker 异常退出的用户可见原因。
///
/// `blocking_budget` 的 `Worker` detail 与 `JoinError` 都是英文实现文案，只进日志，
/// 不回给用户；两处共用同一句，避免文案漂移。
const WORKER_FAILURE_MESSAGE: &str = "任务执行异常，请稍后重试";

/// 预算类失败的可读中文原因（用户可见文案，不外泄英文实现细节）。
pub(super) fn budget_failure_message(error: &BlockingError) -> String {
    match error {
        BlockingError::Busy => "搜索并发已满，请稍后重试".to_string(),
        BlockingError::Timeout => format!("数据源响应超时(>{}秒)", SEARCH_DEADLINE.as_secs()),
        BlockingError::Worker(_) => WORKER_FAILURE_MESSAGE.to_string(),
    }
}

/// 已 spawn 但尚未正常回报的站点身份。
///
/// 任务正常结束时会把自己从列表里移除；panic/abort 的任务不会，于是收尾时能把
/// 这些站点如实记为失败，而不是静默丢弃（AGENTS.md：不要写静默代码）。
#[derive(Default)]
struct UnreportedSites(std::sync::Mutex<Vec<(String, String)>>);

impl UnreportedSites {
    fn mark_spawned(&self, site_id: &str, site_name: &str) {
        self.0
            .lock()
            .unwrap()
            .push((site_id.to_string(), site_name.to_string()));
    }

    fn mark_reported(&self, site_id: &str) {
        self.0.lock().unwrap().retain(|(id, _)| id != site_id);
    }

    fn drain(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.0.lock().unwrap())
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
///
/// 派生前先 `mark_spawned`，任务正常返回前 `mark_reported`；panic/abort 的任务
/// 不会销账，调用方收尾时按站点失败如实上报，而不是静默丢弃。
fn spawn_site_search(
    tasks: &mut tokio::task::JoinSet<SiteSearchResult>,
    state: &Arc<ApiState>,
    site: domain::Site,
    keyword: &str,
    page: u32,
    categories: &[String],
    budget: Duration,
    unreported: &Arc<UnreportedSites>,
) {
    let pool = Arc::clone(&state.search_work);
    let indexer = Arc::clone(&state.indexer);
    let keyword = keyword.to_string();
    let categories = categories.to_vec();
    let site_id = site.id.to_string();
    let site_name = site.name.clone();
    let site_for_search = site.clone();
    let unreported = Arc::clone(unreported);
    unreported.mark_spawned(&site_id, &site_name);
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
        // 正常返回前销账；只有 panic/abort 的任务会留在 unreported 里。
        unreported.mark_reported(&site_id);
        SiteSearchResult {
            site_id,
            site_name,
            outcome,
            elapsed_ms: started.elapsed().as_millis() as i64,
        }
    });
}

/// 收尾时把「派生了但从未正常回报」的站点如实记为单站失败。
///
/// 只应在 JoinSet 已排空时调用：客户端断开时我们主动中断了在飞任务，那些不是
/// 异常退出，谎报成站点失败会污染 JSON/SSE 报告与落盘快照。
async fn report_unreported_sites(
    statuses: &mut Vec<Value>,
    tx: Option<&mpsc::Sender<Result<Event, Infallible>>>,
    unreported: &UnreportedSites,
) {
    for (site_id, site_name) in unreported.drain() {
        tracing::error!(site_id = %site_id, site_name = %site_name, "站点搜索任务异常退出");
        let message = WORKER_FAILURE_MESSAGE.to_string();
        statuses.push(site_status_json(
            &site_id,
            &site_name,
            0,
            Some(message.clone()),
            0,
        ));
        if let Some(tx) = tx {
            send_site_result(
                tx,
                "site_error",
                &site_id,
                &site_name,
                0,
                Some(message),
                0,
                Vec::new(),
            )
            .await;
        }
    }
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
    let unreported = Arc::new(UnreportedSites::default());
    let deadline = Instant::now() + SEARCH_DEADLINE;
    let mut statuses = Vec::new();
    let mut items_all = Vec::new();
    let mut total = 0i64;
    loop {
        if tx.is_some_and(|tx| tx.is_closed()) {
            break;
        }
        // 每次补位前重新采样可用 permit（P3-4）：窗口不是请求开始时的一次性快照。
        // 否则别的请求——或超时后仍持有 permit 的 blocking 闭包——占住槽位时，
        // 本请求的队尾会被自己记住的旧窗口饿死。
        // 跨请求语义：permit 是全局共享的，本请求只承诺「用此刻能拿到的额度补位」；
        // 一个站点若此刻拿不到 permit 仍立即降级为单站失败，不排队等待
        // （见 tests/request_admission.rs::site_over_capacity_degrades_to_per_site_failure）。
        while tasks.len() < state.search_work.available_permits().max(1) {
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
            spawn_site_search(
                &mut tasks,
                state,
                site,
                keyword,
                page,
                categories,
                remaining,
                &unreported,
            );
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
    if tasks.is_empty() {
        report_unreported_sites(&mut statuses, tx, &unreported).await;
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
///
/// 与 `collect_site_results` 相同，窗口在每个数据源派生前重新采样：permit 全局共享，
/// 本请求只按此刻可用额度补位，拿不到额度的数据源立即按单源失败上报。
pub(super) async fn search_title_sources(
    state: &Arc<ApiState>,
    sources: Vec<String>,
    keyword: &str,
) -> (Vec<Value>, Vec<Value>) {
    let mut set: tokio::task::JoinSet<TitleSourceOutcome> = tokio::task::JoinSet::new();
    let mut pending = sources.into_iter();
    let deadline = Instant::now() + SEARCH_DEADLINE;
    let mut titles = Vec::new();
    let mut providers = Vec::new();
    loop {
        while set.len() < state.search_work.available_permits().max(1) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// axum 的 `Event` 没有公开的 data 访问器，`Debug` 输出把 SSE 字节串里的非 ASCII
    /// 字节转义成 `\xNN`；这里还原回文本，用来断言事件内容。
    fn debug_text(event: &Event) -> String {
        let rendered = format!("{event:?}");
        let bytes = rendered.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\' && i + 3 < bytes.len() && bytes[i + 1] == b'x' {
                if let Ok(byte) = u8::from_str_radix(&rendered[i + 2..i + 4], 16) {
                    out.push(byte);
                    i += 4;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// 派生了但从未正常回报的站点必须在收尾时如实记为失败，而不是静默丢弃；
    /// 正常回报过的站点不得重复上报。
    #[tokio::test]
    async fn unreported_site_is_reported_as_failure() {
        let unreported = UnreportedSites::default();
        unreported.mark_spawned("s1", "站点一");
        unreported.mark_spawned("s2", "站点二");
        unreported.mark_reported("s2");

        let mut statuses = Vec::new();
        report_unreported_sites(&mut statuses, None, &unreported).await;

        assert_eq!(statuses.len(), 1, "只有未回报的站点进报告: {statuses:?}");
        assert_eq!(statuses[0]["site_id"], "s1");
        assert_eq!(statuses[0]["site_name"], "站点一");
        assert_eq!(statuses[0]["count"], 0);
        assert_eq!(statuses[0]["error"], WORKER_FAILURE_MESSAGE);
        // drain 是破坏性的：第二次收尾不得重复上报同一站点。
        assert!(unreported.drain().is_empty());
    }

    /// SSE 路径必须为异常退出的站点推一条 site_error，与 JSON 的 sites 报告一致。
    #[tokio::test]
    async fn unreported_site_emits_sse_site_error() {
        let (tx, mut rx) = mpsc::channel(4);
        let unreported = UnreportedSites::default();
        unreported.mark_spawned("s1", "站点一");

        let mut statuses = Vec::new();
        report_unreported_sites(&mut statuses, Some(&tx), &unreported).await;
        drop(tx);

        assert_eq!(statuses.len(), 1);
        let event = rx.recv().await.expect("应推送一条事件").unwrap();
        let rendered = debug_text(&event);
        assert!(rendered.contains("site_error"), "{rendered}");
        assert!(rendered.contains(WORKER_FAILURE_MESSAGE), "{rendered}");
    }
}
