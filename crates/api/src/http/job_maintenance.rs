//! 维护任务的 HTTP 配置与预览（DB8 第 4.2 节 / Task 5）。
//!
//! - `PUT /jobs/{id}/maintenance-config`：按 `def.kind` 用 typed policy 严格校验 body，
//!   事务内 CAS 更新该 def 的 payload；并发令牌是**保存前的原 payload**，
//!   不是 payload 内的 schema `version`。
//! - `POST /jobs/{id}/maintenance-preview`：**有预算的只读预览**。只做有界计数，
//!   不写 def、不创建删除 Job、不改数据库内容，也不返回任何用户播放明细。
//!
//! 两个端点都挂在 admin 路由（`http::admin_ops_routes`），会员在中间件层被拒。

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::JobDefId;
use jobs::JobKind;
use serde::Deserialize;
use serde_json::{Value, json};
use std::str::FromStr;

use crate::http::{err, ok};
use crate::maintenance::{
    BATCH_SIZE, MaintenancePolicy, validate_maintenance_interval, validate_retention_days,
};
use crate::management::ApiState;

const DAY_SECS: i64 = 86_400;

#[derive(Deserialize)]
pub(crate) struct MaintenanceConfigInput {
    /// 版本化维护策略（按 `def.kind` 反序列化；未知字段/未知版本/越界一律 400）。
    payload: Value,
    /// 并发令牌：保存前从 `GET /jobs` 读到的 `payload`（必填；缺省 400）。
    /// 与服务端当前值不符 → 409；这里**不**用 payload 内的 schema `version` 做 CAS。
    expected_payload: Option<Value>,
    /// 维护任务的执行周期（秒），范围 3600..=31536000；其他任务契约不变。
    interval_secs: Option<u64>,
}

#[derive(Deserialize)]
pub(crate) struct MaintenancePreviewInput {
    /// 待预览的策略；缺省时用该 def 当前已保存的 payload。
    payload: Option<Value>,
}

fn parse_def_id(raw: &str) -> Result<JobDefId, Response> {
    JobDefId::from_str(raw).map_err(|_| err(StatusCode::BAD_REQUEST, "job.invalid", "任务 id 无效"))
}

fn load_def(state: &ApiState, id: JobDefId) -> Result<jobs::JobDef, Response> {
    match state.jobs.lock().def(id) {
        Ok(Some(def)) => Ok(def),
        Ok(None) => Err(err(StatusCode::NOT_FOUND, "job.missing", "任务不存在")),
        Err(error) => Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        )),
    }
}

/// 维护 def 才允许配置/预览；其他 kind 一律 400（不是 404：def 存在但用错了端点）。
fn require_maintenance(def: &jobs::JobDef) -> Result<(), Response> {
    if def.kind.is_maintenance() {
        Ok(())
    } else {
        Err(err(
            StatusCode::BAD_REQUEST,
            "job.not_maintenance",
            "该任务不是维护任务，不能用维护配置接口",
        ))
    }
}

/// 解析请求里的策略：坏 JSON/坏字段一律 400，且**不落库**。
fn parse_policy(kind: JobKind, payload: &Value) -> Result<MaintenancePolicy, Response> {
    MaintenancePolicy::parse(kind, &payload.to_string()).map_err(|message| {
        tracing::warn!(kind = kind.as_str(), %message, "维护配置被拒绝");
        err(StatusCode::BAD_REQUEST, "job.invalid_config", &message)
    })
}

pub(crate) async fn update_maintenance_config(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<MaintenanceConfigInput>,
) -> Response {
    let id = match parse_def_id(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let def = match load_def(&state, id) {
        Ok(def) => def,
        Err(response) => return response,
    };
    if let Err(response) = require_maintenance(&def) {
        return response;
    }
    if let Err(message) = parse_policy(def.kind, &body.payload) {
        return message;
    }
    if let Some(secs) = body.interval_secs {
        if let Err(message) = validate_maintenance_interval(secs) {
            return err(StatusCode::BAD_REQUEST, "job.invalid_interval", &message);
        }
    }

    let Some(expected) = body.expected_payload.as_ref() else {
        return err(
            StatusCode::BAD_REQUEST,
            "job.missing_token",
            "缺少 expected_payload 并发令牌：请先读取当前 payload 再提交",
        );
    };
    let new_payload = body.payload.to_string();
    let expected = expected.to_string();
    let new_schedule = body
        .interval_secs
        .map(|secs| jobs::Schedule::Interval { secs });
    let schedule_ref = new_schedule.as_ref();
    let new_schedule_param = body.interval_secs.map(|_| schedule_ref);

    let outcome = match state
        .jobs
        .lock()
        .update_def_config_cas(id, &expected, &new_payload, new_schedule_param)
    {
        Ok(outcome) => outcome,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "job.payload_update_failed",
                &error.to_string(),
            );
        }
    };
    match outcome {
        jobs::PayloadUpdate::Updated => {}
        jobs::PayloadUpdate::Missing => {
            return err(StatusCode::NOT_FOUND, "job.missing", "任务不存在");
        }
        jobs::PayloadUpdate::Conflict => {
            return err(
                StatusCode::CONFLICT,
                "job.conflict",
                "维护配置已被其他请求修改，请刷新后重试",
            );
        }
    }

    // 重新从库中读取实际生效的 schedule，避免在未修改 interval_secs 时返回错误 null
    let active_def = load_def(&state, id).ok();
    let schedule = active_def
        .and_then(|d| d.schedule)
        .and_then(|s| match s {
            jobs::Schedule::Interval { secs } => Some(json!({ "interval_secs": secs })),
        });
    tracing::info!(
        job_def_id = %id,
        kind = def.kind.as_str(),
        interval_secs = body.interval_secs,
        "维护配置已保存"
    );
    ok(json!({
        "id": id.to_string(),
        "kind": def.kind.as_str(),
        "payload": body.payload,
        // 保存后的修改令牌：下一次 CAS 回传这个值。
        "payload_token": new_payload,
        "schedule": schedule,
    }))
    .into_response()
}

pub(crate) async fn preview_maintenance(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<MaintenancePreviewInput>,
) -> Response {
    let id = match parse_def_id(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let def = match load_def(&state, id) {
        Ok(def) => def,
        Err(response) => return response,
    };
    if let Err(response) = require_maintenance(&def) {
        return response;
    }
    let payload = body
        .payload
        .unwrap_or_else(|| serde_json::from_str(&def.payload).unwrap_or_else(|_| json!({})));
    let policy = match parse_policy(def.kind, &payload) {
        Ok(policy) => policy,
        Err(response) => return response,
    };
    let now = crate::job_loop::unix_now();
    let state_for_blocking = state.clone();
    let preview_result = state
        .http_work
        .run(std::time::Duration::from_secs(10), move || {
            match &policy {
                MaintenancePolicy::Jobs(policy) => {
                    let cutoff = cutoff(now, policy.retention_days)
                        .map_err(|e| (StatusCode::BAD_REQUEST, "job.invalid_config", e))?;
                    let batch = state_for_blocking
                        .jobs
                        .lock()
                        .prune_terminal_jobs(cutoff, BATCH_SIZE, true)
                        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, "job.preview_failed", e.to_string()))?;
                    let truncated = batch.has_more || batch.protected_truncated;
                    Ok(json!({
                        "kind": def.kind.as_str(),
                        "policy_version": policy.version,
                        "retention_days": policy.retention_days,
                        "cutoff": cutoff,
                        "scanned": batch.scanned,
                        "candidates": batch.candidates,
                        "protected": batch.protected,
                        "protected_truncated": batch.protected_truncated,
                        "truncated": truncated,
                        "note": "只读预览：只统计服务端固定预算窗口内的候选，不执行任何删除。",
                    }))
                }
                MaintenancePolicy::PlaybackHistory(policy) => {
                    let cutoff = cutoff(now, policy.retention_days)
                        .map_err(|e| (StatusCode::BAD_REQUEST, "job.invalid_config", e))?;
                    let preview = state_for_blocking
                        .store
                        .lock()
                        .preview_playback_logs(cutoff, BATCH_SIZE)
                        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, "job.preview_failed", e.to_string()))?;
                    Ok(json!({
                        "kind": def.kind.as_str(),
                        "policy_version": policy.version,
                        "retention_days": policy.retention_days,
                        "cutoff": cutoff,
                        "scanned": preview.scanned,
                        "candidates": preview.candidates,
                        "protected": preview.protected,
                        "truncated": preview.truncated,
                        "note": "只读预览：只统计保留窗口外的播放历史行数，不返回任何播放明细，也不删除。",
                    }))
                }
                MaintenancePolicy::Database(policy) => Ok(json!({
                    "kind": def.kind.as_str(),
                    "policy_version": policy.version,
                    "scanned": 0,
                    "candidates": 0,
                    "protected": 0,
                    "truncated": false,
                    "note": "数据库维护按需 optimize / VACUUM，没有可预览的删除候选。",
                })),
            }
        })
        .await;

    match preview_result {
        Ok(Ok(preview)) => ok(preview).into_response(),
        Ok(Err((code, err_code, msg))) => err(code, err_code, &msg),
        Err(crate::blocking_budget::BlockingError::Busy) => {
            err(StatusCode::SERVICE_UNAVAILABLE, "runtime.busy", "服务繁忙，请稍后重试预览")
        }
        Err(crate::blocking_budget::BlockingError::Timeout) => {
            err(StatusCode::GATEWAY_TIMEOUT, "job.preview_timeout", "预览统计超时")
        }
        Err(crate::blocking_budget::BlockingError::Worker(msg)) => {
            err(StatusCode::INTERNAL_SERVER_ERROR, "job.preview_failed", &msg)
        }
    }
}

fn cutoff(now: i64, retention_days: u32) -> Result<i64, String> {
    if let Err(message) = validate_retention_days(retention_days) {
        return Err(message);
    }
    let seconds = i64::from(retention_days)
        .checked_mul(DAY_SECS)
        .ok_or("保留天数换算秒数溢出")?;
    now.checked_sub(seconds)
        .ok_or_else(|| format!("保留窗口下界溢出: now={now} seconds={seconds}"))
}
