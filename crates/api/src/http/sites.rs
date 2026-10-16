//! Sites: configured PT trackers. Credentials plaintext in SQLite
//! (ADR-0003). Boost (刷流) config lives in settings KV per site.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use domain::{Site, SiteId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::str::FromStr;

use crate::blocking_budget::BlockingError;
use crate::http::{err, ok};
use crate::management::ApiState;

/// 只读站点请求（验证/上传统计）的等待预算；不替代 adapter 自身的 HTTP 超时。
const SITE_READ_DEADLINE: std::time::Duration = std::time::Duration::from_secs(12);

/// Blocking-budget failure → HTTP response（上游错误走各自原有路径）。
fn blocking_error(error: BlockingError, code: &'static str) -> Response {
    match error {
        BlockingError::Busy => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请重试",
        ),
        BlockingError::Timeout => err(StatusCode::GATEWAY_TIMEOUT, code, "站点请求超时"),
        error => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "runtime.worker",
            &error.to_string(),
        ),
    }
}

#[derive(Deserialize)]
pub(crate) struct SiteInput {
    name: String,
    url: String,
    profile_id: String,
    #[serde(default)]
    cookie: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    rss_url: Option<String>,
    #[serde(default)]
    proxy: Option<String>,
    #[serde(default)]
    rate_limit_per_minute: Option<u32>,
    #[serde(default)]
    cdp_url: Option<String>,
    #[serde(default)]
    downloader_id: Option<String>,
    #[serde(default)]
    maintenance_via: Option<String>,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
pub(crate) struct PatchSiteInput {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    cookie: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    rss_url: Option<String>,
    #[serde(default)]
    proxy: Option<String>,
    #[serde(default)]
    rate_limit_per_minute: Option<u32>,
    #[serde(default)]
    cdp_url: Option<String>,
    #[serde(default)]
    downloader_id: Option<String>,
    #[serde(default)]
    maintenance_via: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Deserialize)]
pub(crate) struct BoostInput {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    budget_bytes: Option<u64>,
    #[serde(default)]
    hold_days: Option<u32>,
}

/// Cookie/API keys stay masked; URL fields are only exposed to administrators.
fn site_json(site: &Site) -> Value {
    json!({
        "id": site.id.to_string(),
        "name": site.name,
        "url": site.url,
        "profile_id": site.profile_id,
        "cookie": null,
        "api_key": null,
        "rss_url": site.rss_url,
        "proxy": site.proxy,
        "rate_limit_per_minute": site.rate_limit_per_minute,
        "cdp_url": site.cdp_url,
        "downloader_id": site.downloader_id.map(|id| id.to_string()),
        "enabled": site.enabled,
        "maintenance_via": site.maintenance_via.as_str(),
    })
}

fn site_public(site: &Site, auth_type: &str, admin: bool) -> Value {
    let mut v = site_json(site);
    v["auth_type"] = json!(auth_type);
    if !admin {
        // Secrets can occur in arbitrary path/query/fragment components, not just userinfo.
        v["url"] = json!("");
        for key in ["rss_url", "proxy", "cdp_url"] {
            v[key] = Value::Null;
        }
    }
    v
}

fn auth_type(site: &Site) -> &'static str {
    if site.api_key.as_deref().is_some_and(|k| !k.is_empty()) {
        "apikey"
    } else if site.cookie.as_deref().is_some_and(|c| !c.is_empty()) {
        "cookie"
    } else {
        "none"
    }
}

async fn create_site_inner(
    State(state): State<ApiState>,
    Json(body): Json<SiteInput>,
) -> Response {
    if state.indexer.profile(&body.profile_id).is_none() {
        return err(
            StatusCode::BAD_REQUEST,
            "site.unknown_profile",
            &format!("未知的站点模板 {}", body.profile_id),
        );
    }
    let profile = state.indexer.profile(&body.profile_id);
    let url = if body.url.trim().is_empty() {
        match profile.and_then(|p| p.base_url.clone()) {
            Some(base) if !base.is_empty() => base,
            _ => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "site.invalid",
                    "该站点模板未内置基底地址，站点 URL 不能为空",
                );
            }
        }
    } else {
        body.url
    };
    let downloader_id = body
        .downloader_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .and_then(|id| domain::DownloaderId::from_str(id).ok());
    let maintenance_via = match body.maintenance_via.as_deref() {
        Some(s) => match s.parse::<domain::MaintenanceVia>() {
            Ok(v) => v,
            Err(_) => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "site.invalid_maintenance_via",
                    "无效的 maintenance_via",
                );
            }
        },
        None => domain::MaintenanceVia::Http,
    };
    if maintenance_via == domain::MaintenanceVia::Browser
        && body.proxy.as_deref().is_some_and(|p| !p.trim().is_empty())
    {
        return err(
            StatusCode::BAD_REQUEST,
            "site.browser_proxy_unsupported",
            "浏览器维护方式不支持配置代理",
        );
    }
    if let Some(cdp_url) = body.cdp_url.as_deref().filter(|u| !u.trim().is_empty()) {
        if let Err(e) = indexer::BrowserConfig::validate_endpoint(cdp_url.trim()) {
            return err(
                StatusCode::BAD_REQUEST,
                "site.invalid_endpoint",
                &e.to_string(),
            );
        }
    }
    let site = Site {
        id: SiteId::new(),
        name: body.name,
        url,
        profile_id: body.profile_id,
        cookie: body.cookie,
        api_key: body.api_key,
        rss_url: body.rss_url,
        proxy: body.proxy,
        rate_limit_per_minute: body.rate_limit_per_minute,
        cdp_url: body.cdp_url,
        downloader_id,
        enabled: body.enabled,
        maintenance_via,
    };
    let store = state.store.lock();
    if let Err(error) = store.insert_site(&site) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    let mut view = site_public(&site, auth_type(&site), true);
    view["cookie"] = site
        .cookie
        .clone()
        .map(Value::String)
        .unwrap_or(Value::Null);
    view["api_key"] = site
        .api_key
        .clone()
        .map(Value::String)
        .unwrap_or(Value::Null);
    (StatusCode::CREATED, ok(view)).into_response()
}

pub(crate) async fn list_sites(
    State(state): State<ApiState>,
    Extension(user_id): Extension<domain::UserId>,
) -> Response {
    let store = state.store.lock();
    let sites = match store.list_sites() {
        Ok(sites) => sites,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let admin = site_admin(&store, user_id);
    ok(Value::Array(
        sites
            .iter()
            .map(|site| site_public(site, auth_type(site), admin))
            .collect(),
    ))
    .into_response()
}

async fn put_site_inner(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<SiteInput>,
) -> Response {
    if state.indexer.profile(&body.profile_id).is_none() {
        return err(
            StatusCode::BAD_REQUEST,
            "site.invalid",
            &format!("未知站点配置: {}", body.profile_id),
        );
    }
    let site_id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let downloader_id = match body
        .downloader_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .map(domain::DownloaderId::from_str)
        .transpose()
    {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "下载器 id 无效"),
    };
    let store = state.store.lock();
    let current = match store.get_site(site_id) {
        Ok(Some(site)) => site,
        Ok(None) => return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在"),
        Err(error) => return site_store_error(&error),
    };
    let site = match replacement_site(current, body, downloader_id) {
        Ok(site) => site,
        Err((status, code, msg)) => return err(status, code, msg),
    };
    let updated = match store.save_site(&site) {
        Ok(updated) => updated,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    if !updated {
        return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在");
    }
    ok(site_json(&site)).into_response()
}

/// Null/absent masked credentials mean unchanged; an explicit empty string clears them.
fn replacement_site(
    current: Site,
    body: SiteInput,
    downloader_id: Option<domain::DownloaderId>,
) -> Result<Site, (StatusCode, &'static str, &'static str)> {
    let maintenance_via = match body.maintenance_via.as_deref() {
        Some(s) => match s.parse::<domain::MaintenanceVia>() {
            Ok(v) => v,
            Err(_) => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "site.invalid_maintenance_via",
                    "无效的 maintenance_via",
                ));
            }
        },
        None => current.maintenance_via,
    };
    let proxy = replacement_credential(body.proxy, current.proxy);
    if maintenance_via == domain::MaintenanceVia::Browser
        && proxy.as_deref().is_some_and(|p| !p.trim().is_empty())
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "site.browser_proxy_unsupported",
            "浏览器维护方式不支持配置代理",
        ));
    }
    let cdp_url = replacement_credential(body.cdp_url, current.cdp_url);
    if let Some(endpoint) = cdp_url.as_deref().filter(|u| !u.trim().is_empty()) {
        if let Err(_) = indexer::BrowserConfig::validate_endpoint(endpoint.trim()) {
            return Err((
                StatusCode::BAD_REQUEST,
                "site.invalid_endpoint",
                "无效的 CDP discovery 端点",
            ));
        }
    }
    Ok(Site {
        id: current.id,
        name: body.name,
        url: body.url,
        profile_id: body.profile_id,
        cookie: replacement_credential(body.cookie, current.cookie),
        api_key: replacement_credential(body.api_key, current.api_key),
        rss_url: replacement_credential(body.rss_url, current.rss_url),
        proxy,
        rate_limit_per_minute: body.rate_limit_per_minute,
        cdp_url,
        downloader_id,
        enabled: body.enabled,
        maintenance_via,
    })
}

fn replacement_credential(input: Option<String>, current: Option<String>) -> Option<String> {
    match input {
        Some(value) => (!value.is_empty()).then_some(value),
        None => current,
    }
}

fn site_store_error(error: &impl std::fmt::Display) -> Response {
    tracing::error!(%error, "Site storage operation failed");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}

fn site_admin(store: &crate::Store, user_id: domain::UserId) -> bool {
    match store.user_role(user_id) {
        Ok(role) => role == "admin",
        Err(error) => {
            tracing::error!(%user_id, %error, "Failed to resolve Site credential access");
            false
        }
    }
}

pub(crate) async fn get_site(
    State(state): State<ApiState>,
    Extension(user_id): Extension<domain::UserId>,
    Path(id): Path<String>,
) -> Response {
    let id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let store = state.store.lock();
    let Some(site) = store.get_site(id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在");
    };
    ok(site_public(
        &site,
        auth_type(&site),
        site_admin(&store, user_id),
    ))
    .into_response()
}

async fn patch_site_inner(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<PatchSiteInput>,
) -> Response {
    let id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let store = state.store.lock();
    let Some(mut site) = store.get_site(id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在");
    };
    if let Some(name) = body.name.filter(|n| !n.trim().is_empty()) {
        site.name = name;
    }
    if let Some(url) = body.url.filter(|u| !u.trim().is_empty()) {
        site.url = url;
    }
    if let Some(cookie) = body.cookie {
        site.cookie = (!cookie.is_empty()).then_some(cookie);
    }
    if let Some(api_key) = body.api_key {
        site.api_key = (!api_key.is_empty()).then_some(api_key);
    }
    if let Some(rss_url) = body.rss_url {
        site.rss_url = (!rss_url.is_empty()).then_some(rss_url);
    }
    if let Some(proxy) = body.proxy {
        site.proxy = (!proxy.is_empty()).then_some(proxy);
    }
    if let Some(rate) = body.rate_limit_per_minute {
        site.rate_limit_per_minute = (rate > 0).then_some(rate);
    }
    if let Some(cdp_url) = body.cdp_url {
        let trimmed = cdp_url.trim();
        if !trimmed.is_empty() {
            if let Err(_) = indexer::BrowserConfig::validate_endpoint(trimmed) {
                return err(
                    StatusCode::BAD_REQUEST,
                    "site.invalid_endpoint",
                    "无效的 CDP discovery 端点",
                );
            }
        }
        site.cdp_url = (!cdp_url.is_empty()).then_some(cdp_url);
    }
    if let Some(downloader_id) = body.downloader_id {
        site.downloader_id = (!downloader_id.is_empty())
            .then(|| domain::DownloaderId::from_str(&downloader_id).ok())
            .flatten();
    }
    if let Some(mv_str) = body.maintenance_via {
        match mv_str.parse::<domain::MaintenanceVia>() {
            Ok(v) => site.maintenance_via = v,
            Err(_) => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "site.invalid_maintenance_via",
                    "无效的 maintenance_via",
                );
            }
        }
    }
    if let Some(enabled) = body.enabled {
        site.enabled = enabled;
    }
    if site.maintenance_via == domain::MaintenanceVia::Browser
        && site.proxy.as_deref().is_some_and(|p| !p.trim().is_empty())
    {
        return err(
            StatusCode::BAD_REQUEST,
            "site.browser_proxy_unsupported",
            "浏览器维护方式不支持配置代理",
        );
    }
    if let Err(error) = store.save_site(&site) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    ok(site_public(&site, auth_type(&site), true)).into_response()
}

async fn delete_site_inner(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let store = state.store.lock();
    match store.delete_site(id) {
        Ok(true) => {
            tracing::info!(site_id = %id, "已删除索引站点");
            ok(json!({ "deleted": true })).into_response()
        }
        _ => {
            tracing::warn!(site_id = %id, "待删除的站点不存在");
            err(StatusCode::NOT_FOUND, "site.missing", "站点不存在")
        }
    }
}

pub(crate) async fn verify_site(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let site = {
        let store = state.store.lock();
        let Some(site) = store.get_site(id).ok().flatten() else {
            return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在");
        };
        site
    };
    let state_for_work = state.clone();
    let outcome = state
        .http_work
        .run(SITE_READ_DEADLINE, move || {
            state_for_work.indexer.search(std::slice::from_ref(&site), " ")
        })
        .await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => return blocking_error(error, "site.verify_timeout"),
    };
    let has_hits = !outcome.torrents.is_empty();
    let error = outcome
        .failures
        .into_iter()
        .next()
        .map(|failure| failure.error)
        .filter(|_| !has_hits);
    ok(json!({
        "ok": has_hits || error.is_none(),
        "error": error,
        "torrent_count": outcome.torrents.len(),
    }))
    .into_response()
}

pub(crate) async fn site_login(State(state): State<ApiState>, Path(id): Path<String>) -> Response {
    let site_id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let Some(site) = state.store.lock().get_site(site_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在");
    };
    // 借用 adapter（StoreSites / AttendanceHttp / LoginPlugin）在闭包内构造，
    // 避免把没有 Send+Sync 约束的 HttpPost trait object 送进 spawn。
    let state_for_work = state.clone();
    let result = state
        .http_work
        .run_to_completion(move || -> Result<(), (StatusCode, &'static str, String)> {
            let policy = crate::check_in::site_policy(&state_for_work, &site).map_err(|error| {
                (
                    StatusCode::BAD_REQUEST,
                    "site.maintenance_unsupported",
                    error.to_string(),
                )
            })?;
            let bus = hooks::Bus::new();
            let creds = crate::check_in::store_sites(&state_for_work.store);
            crate::site_maintenance::perform_login(&state_for_work, &site, &bus, &creds)
                .map_err(|error| {
                    (
                        StatusCode::BAD_REQUEST,
                        "site.login_failed",
                        error.to_string(),
                    )
                })
        })
        .await;
    match result {
        Ok(Ok(())) => ok(json!({ "ok": true })).into_response(),
        Ok(Err((status, code, message))) => err(status, code, &message),
        Err(error) => blocking_error(error, "site.login_timeout"),
    }
}

pub(crate) async fn site_check_in(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let site_id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let Some(site) = state.store.lock().get_site(site_id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在");
    };
    let state_for_work = state.clone();
    let result = state
        .http_work
        .run_to_completion(move || -> Result<(), (StatusCode, &'static str, String)> {
            let policy = crate::check_in::site_policy(&state_for_work, &site).map_err(|error| {
                (
                    StatusCode::BAD_REQUEST,
                    "site.maintenance_unsupported",
                    error.to_string(),
                )
            })?;
            let bus = hooks::Bus::new();
            let creds = crate::check_in::store_sites(&state_for_work.store);
            crate::site_maintenance::perform_check_in(&state_for_work, &site, &bus, &creds)
                .map_err(|error| {
                    (
                        StatusCode::BAD_REQUEST,
                        "site.check_in_failed",
                        error.to_string(),
                    )
                })
        })
        .await;
    match result {
        Ok(Ok(())) => ok(json!({ "ok": true })).into_response(),
        Ok(Err((status, code, message))) => err(status, code, &message),
        Err(error) => blocking_error(error, "site.check_in_timeout"),
    }
}

fn boost_key(id: &str) -> String {
    format!("boost.{}", id)
}

pub(crate) async fn patch_boost(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<BoostInput>,
) -> Response {
    let id = match SiteId::from_str(&id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效"),
    };
    let store = state.store.lock();
    if store.get_site(id).ok().flatten().is_none() {
        return err(StatusCode::NOT_FOUND, "site.missing", "站点不存在");
    }
    let key = boost_key(&id.to_string());
    let current = store
        .get_setting(&key)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}));
    let mut next = current;
    if let Some(enabled) = body.enabled {
        next["enabled"] = json!(enabled);
    }
    if let Some(budget) = body.budget_bytes {
        next["budget_bytes"] = json!(budget);
    }
    if let Some(hold) = body.hold_days {
        next["hold_days"] = json!(hold);
    }
    if let Err(error) = store.put_setting(&key, &next.to_string()) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    ok(json!({
        "site_id": id.to_string(),
        "boost": next,
    }))
    .into_response()
}

pub(crate) async fn boost_stats(State(state): State<ApiState>) -> Response {
    let (rows, category) = {
        let store = state.store.lock();
        let sites = match store.list_sites() {
            Ok(sites) => sites,
            Err(error) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        };
        let rows: Vec<Value> = sites
            .iter()
            .map(|site| {
                let raw = store
                    .get_setting(&boost_key(&site.id.to_string()))
                    .ok()
                    .flatten()
                    .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                    .unwrap_or_else(|| json!({ "enabled": false }));
                json!({
                    "site_id": site.id.to_string(),
                    "site_name": site.name,
                    "boost": raw,
                })
            })
            .collect();
        // Live upload totals from the configured downloader (best-effort).
        let category = store
            .default_downloader()
            .ok()
            .flatten()
            .and_then(|d| d.category)
            .unwrap_or_default();
        (rows, category)
    };
    // 上传统计是 best-effort telemetry：worker 忙/超时/上游失败都按 0 处理，
    // 不让 /boost-stats 因为遥测而改变响应契约。
    let state_for_work = state.clone();
    let uploaded = match state
        .http_work
        .run(SITE_READ_DEADLINE, move || {
            state_for_work.downloader.uploaded_by_category(&category)
        })
        .await
    {
        Ok(Ok(uploaded)) => uploaded,
        Ok(Err(error)) => {
            tracing::warn!(%error, "boost 上传统计读取失败，按 0 处理");
            0
        }
        Err(error) => {
            tracing::warn!(%error, "boost 上传统计 worker 未完成，按 0 处理");
            0
        }
    };
    let mut out = rows;
    for row in &mut out {
        row["uploaded_bytes"] = json!(uploaded);
    }
    ok(Value::Array(out)).into_response()
}

pub(crate) async fn site_catalog(State(state): State<ApiState>) -> Response {
    // Profiles are compile-time + overlay; expose id/name only.
    // demo 是测试桩，不列进站点配置。
    let items: Vec<Value> = state
        .indexer
        .profile_ids()
        .into_iter()
        .filter(|id| id != "demo")
        .map(|id| {
            json!({
                "profile_id": id,
                "auth_types": if id == "mteam" { ["apikey"] } else { ["cookie"] },
            })
        })
        .collect();
    ok(Value::Array(items)).into_response()
}

pub(crate) async fn create_site(
    State(state): State<ApiState>,
    Json(body): Json<SiteInput>,
) -> Response {
    after_site_write(&state, create_site_inner(State(state.clone()), Json(body)).await)
}

pub(crate) async fn put_site(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<SiteInput>,
) -> Response {
    after_site_write(&state, put_site_inner(State(state.clone()), Path(id), Json(body)).await)
}

pub(crate) async fn patch_site(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<PatchSiteInput>,
) -> Response {
    after_site_write(&state, patch_site_inner(State(state.clone()), Path(id), Json(body)).await)
}

pub(crate) async fn delete_site(
    State(state): State<ApiState>, Path(id): Path<String>
) -> Response {
    after_site_write(&state, delete_site_inner(State(state.clone()), Path(id)).await)
}

/// Site writes can change `managed_browser_sites`; re-run the managed-daemon
/// decision in the background after any successful write.
fn after_site_write(state: &ApiState, response: Response) -> Response {
    if response.status().is_success() {
        crate::http::browser_settings::spawn_reconcile(state);
    }
    response
}
