//! Downloaders: qBittorrent / Transmission instances, live tasks, submit.

use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::blocking_budget::BlockingError;
use crate::http::{err, ok};
use crate::management::ApiState;

/// 种子已不在下载器时，pending 行的自动清理宽限期（秒）。
///
/// 提交后的短窗口内种子可能尚未在客户端可见（URL/磁力投递需要时间入库），
/// 宽限期内缺失只如实上报 missing 不删行；超过宽限期且客户端可达仍找不到，
/// 判定为「任务已消失」，自动清理 pending 行（下一轮搜索会按订阅重新投递）。
pub(super) use crate::pending_reconcile::ABSENT_CLEANUP_GRACE_SECS;

#[derive(Deserialize)]
pub(crate) struct SubmitInput {
    pub(super) download_url: String,
    pub(super) title: String,
    #[serde(default)]
    pub(super) site_id: Option<String>,
    #[serde(default)]
    pub(super) torrent_id: Option<String>,
    #[serde(default)]
    pub(super) category: Option<String>,
    #[serde(default)]
    pub(super) publish_time: Option<String>,
    #[serde(default)]
    pub(super) size_bytes: Option<u64>,
    #[serde(default)]
    pub(super) downloader_id: Option<String>,
    /// 投给某个订阅：写 pending + 记 grab，让 Transfer 能收集入库。
    #[serde(default)]
    pub(super) subscribe_id: Option<String>,
    /// Explicit downloader-side directory. For auto_route this is the host-side
    /// path returned by the routing preview and is translated through path_maps.
    #[serde(default)]
    pub(super) save_path: Option<String>,
    #[serde(default)]
    pub(super) auto_route: bool,
    /// D-M5: submit even when the subscription already owns / downloads the slot.
    #[serde(default)]
    pub(super) force: bool,
}

/// 下载器工作失败的两层结果：解析失败 vs 调用失败 vs 预算失败。
pub(super) enum DownloaderWorkError {
    /// `client_for_id` 失败（配置不存在/建连失败）。
    Resolve(String),
    /// 已连上的 client 返回的 DownloaderError。
    Operation(String),
    /// 只读路径的等待预算耗尽（有副作用动作不会出现）。
    Timeout,
    Busy,
    Worker(String),
    /// RPC 已完成，但本地 finalization（如删除 pending 行）失败。
    Finalize(String),
}

impl DownloaderWorkError {
    /// Resolve→502、Operation/Worker→500、Timeout→504、Busy→503、
    /// Finalize→500 store.error（remove_task 另行映射 Resolve）。
    pub(super) fn response(self) -> Response {
        match self {
            Self::Resolve(message) => err(StatusCode::BAD_GATEWAY, "downloader.error", &message),
            Self::Operation(message) | Self::Worker(message) => err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "downloader.error",
                &message,
            ),
            Self::Timeout => err(
                StatusCode::GATEWAY_TIMEOUT,
                "downloader.timeout",
                "下载器响应超时",
            ),
            Self::Busy => err(
                StatusCode::SERVICE_UNAVAILABLE,
                "runtime.busy",
                "服务繁忙，请重试",
            ),
            Self::Finalize(message) => {
                err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &message)
            }
        }
    }
}

/// 解析 client 与随后的 trait 调用必须一起放进同一个 owned blocking 闭包：
/// 非默认下载器的 `client_for_id` 会同步建连，提前 resolve 会阻塞 async worker。
async fn run_downloader<T, F>(
    state: &ApiState,
    downloader_id: Option<domain::DownloaderId>,
    deadline: Option<Duration>,
    work: F,
) -> Result<T, DownloaderWorkError>
where
    T: Send + 'static,
    F: FnOnce(&dyn downloader::Downloader) -> Result<T, downloader::DownloaderError>
        + Send
        + 'static,
{
    let state_for_work = state.clone();
    let job = move || -> Result<T, DownloaderWorkError> {
        let client = crate::delivery::client_for_id(&state_for_work, downloader_id)
            .map_err(|error| DownloaderWorkError::Resolve(error.to_string()))?;
        work(client.as_ref()).map_err(|error| DownloaderWorkError::Operation(error.to_string()))
    };
    let outcome = match deadline {
        Some(deadline) => state.http_work.run(deadline, job).await,
        None => state.http_work.run_to_completion(job).await,
    };
    outcome.unwrap_or_else(|error| match error {
        BlockingError::Busy => Err(DownloaderWorkError::Busy),
        BlockingError::Timeout => Err(DownloaderWorkError::Timeout),
        other => Err(DownloaderWorkError::Worker(other.to_string())),
    })
}

/// 只读查询（telemetry / snapshots）：12s 等待预算。
pub(super) async fn run_downloader_read<T, F>(
    state: &ApiState,
    downloader_id: Option<domain::DownloaderId>,
    work: F,
) -> Result<T, DownloaderWorkError>
where
    T: Send + 'static,
    F: FnOnce(&dyn downloader::Downloader) -> Result<T, downloader::DownloaderError>
        + Send
        + 'static,
{
    run_downloader(state, downloader_id, Some(Duration::from_secs(12)), work).await
}

/// 有副作用动作（remove / delete / pause / resume / set_limits）：必须等到
/// worker 真正结束，绝不"超时返回但后台继续删"。
pub(super) async fn run_downloader_action<T, F>(
    state: &ApiState,
    downloader_id: Option<domain::DownloaderId>,
    work: F,
) -> Result<T, DownloaderWorkError>
where
    T: Send + 'static,
    F: FnOnce(&dyn downloader::Downloader) -> Result<T, downloader::DownloaderError>
        + Send
        + 'static,
{
    run_downloader(state, downloader_id, None, work).await
}

/// 有副作用动作 + 本地 finalization 必须同属一个 owned 作业。
///
/// 请求 future 被 drop 时，blocking 闭包仍会跑完 RPC 并执行 `finalize`，
/// 避免“外部任务已删、本地 pending 还在”的不一致。Store 错误单独成
/// [`DownloaderWorkError::Finalize`]，不塞进 `DownloaderError`。
pub(super) async fn run_downloader_action_then<T, F, G>(
    state: &ApiState,
    downloader_id: Option<domain::DownloaderId>,
    work: F,
    finalize: G,
) -> Result<T, DownloaderWorkError>
where
    T: Send + 'static,
    F: FnOnce(&dyn downloader::Downloader) -> Result<T, downloader::DownloaderError>
        + Send
        + 'static,
    G: FnOnce() -> Result<(), String> + Send + 'static,
{
    let state_for_work = state.clone();
    let job = move || -> Result<T, DownloaderWorkError> {
        let client = crate::delivery::client_for_id(&state_for_work, downloader_id)
            .map_err(|error| DownloaderWorkError::Resolve(error.to_string()))?;
        let value = work(client.as_ref())
            .map_err(|error| DownloaderWorkError::Operation(error.to_string()))?;
        finalize().map_err(DownloaderWorkError::Finalize)?;
        Ok(value)
    };
    match state.http_work.run_to_completion(job).await {
        Ok(result) => result,
        Err(BlockingError::Busy) => Err(DownloaderWorkError::Busy),
        Err(BlockingError::Timeout) => Err(DownloaderWorkError::Timeout),
        Err(other) => Err(DownloaderWorkError::Worker(other.to_string())),
    }
}

/// 只解析 query 里的 downloader_id（纯字符串处理，不触碰 Store/网络）。
fn routed_task_downloader_id(
    query: &HashMap<String, String>,
) -> Result<Option<domain::DownloaderId>, Response> {
    match query.get("downloader_id").filter(|id| !id.is_empty()) {
        Some(raw) => Ok(Some(domain::DownloaderId::from_str(raw).map_err(|_| {
            err(
                StatusCode::BAD_REQUEST,
                "downloader.invalid_id",
                "下载器 ID 不是合法 UUID",
            )
        })?)),
        None => Ok(None),
    }
}

pub(super) fn task_state(raw: &str, progress: f64) -> &'static str {
    match raw {
        "error" => "error",
        "missingFiles" => "missing",
        "stalledDL" => "stalled",
        "downloading" | "forcedDL" | "metaDL" | "forcedMetaDL" | "allocating" => "downloading",
        "queuedDL" | "queuedUP" | "queuedForChecking" => "queued",
        "pausedDL" | "pausedUP" | "stoppedDL" | "stoppedUP" => "paused",
        "checkingDL" | "checkingUP" | "checkingResumeData" | "moving" => "checking",
        // Seeding states: the bytes are complete.
        "uploading" | "stalledUP" | "forcedUP" => "completed",
        _ if progress >= 1.0 => "completed",
        _ => "unknown",
    }
}

/// DELETE /downloaders/tasks/{hash}?delete_files=false
pub(crate) async fn delete_task(
    State(state): State<ApiState>,
    Path(hash): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let delete_files = query.get("delete_files").is_some_and(|v| v == "true");
    let downloader_id = match routed_task_downloader_id(&query) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match run_downloader_action(&state, downloader_id, move |client| {
        client.delete_task(&hash, delete_files)
    })
    .await
    {
        Ok(()) => ok(json!({"ok": true})).into_response(),
        Err(error) => error.response(),
    }
}

/// POST /downloaders/tasks/{hash}/pause
pub(crate) async fn pause_task(
    State(state): State<ApiState>,
    Path(hash): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let downloader_id = match routed_task_downloader_id(&query) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match run_downloader_action(&state, downloader_id, move |client| {
        client.pause_task(&hash)
    })
    .await
    {
        Ok(()) => ok(json!({"ok": true})).into_response(),
        Err(error) => error.response(),
    }
}

/// POST /downloaders/tasks/{hash}/resume
pub(crate) async fn resume_task(
    State(state): State<ApiState>,
    Path(hash): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let downloader_id = match routed_task_downloader_id(&query) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match run_downloader_action(&state, downloader_id, move |client| {
        client.resume_task(&hash)
    })
    .await
    {
        Ok(()) => ok(json!({"ok": true})).into_response(),
        Err(error) => error.response(),
    }
}

// ---------------------------------------------------------------------------
// Subscription-delivered tasks, addressed by the task-center `id`
// ---------------------------------------------------------------------------
//
// `list_tasks` builds each item's `id` as `{subscribe_id}:{enclosure}`. That is
// the only stable handle the task center has: qBittorrent info hashes are not
// known until the torrent is actually in the client, so `info_hash` is null for
// subscribe deliveries. These two endpoints therefore take the task id.

/// Split a task-center id into `(subscribe_id, enclosure)`.
///
/// Enclosures are URLs and therefore contain `:` — always split on the first
/// one so the enclosure survives intact.
fn parse_task_id(raw: &str) -> Option<(&str, &str)> {
    let (subscribe_id, enclosure) = raw.split_once(':')?;
    if subscribe_id.is_empty() || enclosure.is_empty() {
        return None;
    }
    Some((subscribe_id, enclosure))
}

#[derive(Deserialize)]
pub(crate) struct TaskInput {
    task_id: String,
    #[serde(default)]
    delete_files: bool,
}

/// Resolve a task id to its stored Torrent, or explain why it cannot be found.
fn resolve_task(
    store: &crate::Store,
    task_id: &str,
) -> Result<(domain::SubscribeId, crate::store::PendingDownload), String> {
    let (subscribe_id, enclosure) =
        parse_task_id(task_id).ok_or_else(|| "任务 id 无效".to_string())?;
    let subscribe_id =
        domain::SubscribeId::from_str(subscribe_id).map_err(|_| "任务 id 无效".to_string())?;
    let torrent = store
        .list_all_pending_routed()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|(id, _, pending)| *id == subscribe_id && pending.torrent.enclosure == enclosure)
        .map(|(_, _, pending)| pending)
        .ok_or_else(|| "任务不存在或已入库".to_string())?;
    Ok((subscribe_id, torrent))
}

/// POST /downloaders/tasks/remove — remove a subscription-delivered task.
///
/// Unlike `DELETE /downloaders/tasks/{hash}`, this works for tasks whose info
/// hash is unknown: the stored Torrent is matched inside the downloader by tag
/// or name, then the pending row is dropped.
pub(crate) async fn remove_task(
    State(state): State<ApiState>,
    Json(body): Json<TaskInput>,
) -> Response {
    let (subscribe_id, pending) = {
        let store = state.store.lock();
        match resolve_task(&store, &body.task_id) {
            Ok(found) => found,
            Err(message) => return err(StatusCode::NOT_FOUND, "task.missing", &message),
        }
    };
    let delete_files = body.delete_files;
    let torrent_for_remove = pending.torrent.clone();
    let enclosure = pending.torrent.enclosure.clone();
    let state_for_finalize = state.clone();
    // RPC 与本地 pending finalization 同属一个 owned 作业：请求 future 被 drop
    // 时 blocking 闭包仍会跑完 remove_owned 并删除这条 pending（身份=subscribe+enclosure）。
    match run_downloader_action_then(
        &state,
        pending.downloader_id,
        move |client| client.remove_owned(&torrent_for_remove, delete_files),
        move || {
            state_for_finalize
                .store
                .lock()
                .delete_pending(subscribe_id, &enclosure)
                .map(|_| ())
                .map_err(|error| error.to_string())
        },
    )
    .await
    {
        Ok(()) => {}
        Err(DownloaderWorkError::Resolve(message)) => {
            // 保留既有 remove_task 的解析失败状态码（500，而非 routed 路径的 502）。
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "downloader.error",
                &message,
            );
        }
        Err(error) => return error.response(),
    }
    ok(json!({
        "ok": true,
        "subscribe_id": subscribe_id.to_string(),
        "delete_files": delete_files,
    }))
    .into_response()
}

/// POST /downloaders/tasks/replace — search for an alternative source.
///
/// Queues an immediate one-shot `subscribe_search` for the owning Subscribe
/// with the stalled enclosure excluded, so this round can only select a
/// *different* source. The stalled task is deliberately left in place: it keeps
/// seeding/downloading until a replacement actually lands.
pub(crate) async fn replace_task(
    State(state): State<ApiState>,
    Json(body): Json<TaskInput>,
) -> Response {
    let (subscribe_id, pending) = {
        let store = state.store.lock();
        match resolve_task(&store, &body.task_id) {
            Ok(found) => found,
            Err(message) => return err(StatusCode::NOT_FOUND, "task.missing", &message),
        }
    };
    let payload = json!({
        "subscribe_id": subscribe_id.to_string(),
        "exclude_enclosure": pending.torrent.enclosure,
    })
    .to_string();
    let queue = state.jobs.lock();
    let job = match queue.enqueue_with_key(
        jobs::JobKind::SubscribeSearch,
        &payload,
        crate::job_loop::unix_now(),
        Some(&format!("subscribe:{subscribe_id}")),
    ) {
        Ok(job) => job,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "job.enqueue_failed",
                &error.to_string(),
            );
        }
    };
    tracing::info!(
        subscribe_id = %subscribe_id,
        excluded = %pending.torrent.enclosure,
        job_id = %job.id,
        "已为卡住的任务排队换源搜索"
    );
    ok(json!({
        "ok": true,
        "subscribe_id": subscribe_id.to_string(),
        "job_id": job.id.to_string(),
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::{parse_task_id, task_state};

    #[test]
    fn task_id_splits_on_the_first_colon_so_urls_survive() {
        let (subscribe, enclosure) =
            parse_task_id("11111111-1111-1111-1111-111111111111:https://pt.example/dl?id=1")
                .expect("valid task id");
        assert_eq!(subscribe, "11111111-1111-1111-1111-111111111111");
        assert_eq!(enclosure, "https://pt.example/dl?id=1");
    }

    #[test]
    fn task_id_rejects_missing_halves() {
        assert!(parse_task_id("no-colon").is_none());
        assert!(parse_task_id(":https://pt.example/dl").is_none());
        assert!(parse_task_id("sub-1:").is_none());
    }

    #[test]
    fn qbittorrent_states_map_onto_the_task_center_vocabulary() {
        assert_eq!(task_state("stalledDL", 0.4), "stalled");
        assert_eq!(task_state("error", 0.0), "error");
        assert_eq!(task_state("missingFiles", 0.0), "missing");
        assert_eq!(task_state("downloading", 0.4), "downloading");
        assert_eq!(task_state("forcedDL", 0.4), "downloading");
        assert_eq!(task_state("metaDL", 0.0), "downloading");
        assert_eq!(task_state("queuedDL", 0.0), "queued");
        assert_eq!(task_state("pausedDL", 0.4), "paused");
        assert_eq!(task_state("stoppedUP", 1.0), "paused");
        assert_eq!(task_state("checkingDL", 0.0), "checking");
        assert_eq!(task_state("uploading", 1.0), "completed");
        assert_eq!(task_state("stalledUP", 1.0), "completed");
        // Unknown strings with complete bytes are complete; otherwise unknown.
        assert_eq!(task_state("somethingNew", 1.0), "completed");
        assert_eq!(task_state("somethingNew", 0.3), "unknown");
    }
}
