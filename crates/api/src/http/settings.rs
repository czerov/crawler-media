//! Settings: metadata source credentials (TMDB etc.) stored in settings KV.
//! Keys are read live by the catalog clients, so saving here applies at once.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::http::{err, ok};
use crate::management::ApiState;
use crate::settings_keys;

fn masked(key: Option<String>) -> Value {
    match key.filter(|k| !k.is_empty()) {
        Some(k) if k.len() > 8 => json!(format!("{}…{}", &k[..4], &k[k.len() - 4..])),
        Some(_) => json!("已配置"),
        None => Value::Null,
    }
}

/// Metadata source configuration + status.
pub(crate) async fn get_metadata(State(state): State<ApiState>) -> Response {
    let store = state.store.lock();
    let tmdb = store
        .get_setting(settings_keys::TMDB_API_KEY)
        .ok()
        .flatten();
    let tvdb = store
        .get_setting(settings_keys::TVDB_API_KEY)
        .ok()
        .flatten();
    let language = store
        .get_setting(settings_keys::METADATA_LANGUAGE)
        .ok()
        .flatten();
    ok(json!({
        "language": language.unwrap_or_default(),
        "tmdb": {
            "configured": tmdb.as_deref().is_some_and(|k| !k.is_empty()),
            "api_key_hint": masked(tmdb.clone()),
            "env_override": crate::config::tmdb_key_from_env_is_set(),
        },
        "tvdb": {
            "configured": tvdb.as_deref().is_some_and(|k| !k.is_empty()),
            "api_key_hint": masked(tvdb.clone()),
            "env_override": crate::config::is_env_pinned(settings_keys::TVDB_API_KEY),
        },
        "sources": [
            { "id": "tmdb", "name": "TMDB", "needs_key": true, "configured": tmdb.as_deref().is_some_and(|k| !k.is_empty()) },
            { "id": "douban", "name": "豆瓣", "needs_key": false, "configured": true },
            { "id": "tvdb", "name": "TVDB", "needs_key": true, "configured": tvdb.as_deref().is_some_and(|k| !k.is_empty()) },
            { "id": "bangumi", "name": "Bangumi", "needs_key": false, "configured": true },
            { "id": "anilist", "name": "AniList", "needs_key": false, "configured": true },
        ],
    }))
    .into_response()
}

/// Save metadata source keys (empty string clears).
///
/// Keys pinned by the environment (`CRAWLER_MEDIA_TMDB_KEY` / `_TVDB_KEY`) are
/// rejected with 409: they are re-seeded from env on every start, so accepting
/// the write would silently lose it at the next restart.
pub(crate) async fn put_metadata(
    State(state): State<ApiState>,
    Json(body): Json<Value>,
) -> Response {
    for (field, key) in [
        ("tmdb_api_key", settings_keys::TMDB_API_KEY),
        ("tvdb_api_key", settings_keys::TVDB_API_KEY),
    ] {
        if body[field].as_str().is_some() {
            if let Some(message) = crate::config::env_pinned_message(key) {
                return err(StatusCode::CONFLICT, "settings.env_pinned", &message);
            }
        }
    }
    let (tmdb, tvdb) = {
        let store = state.store.lock();
        if let Some(key) = body["tmdb_api_key"].as_str() {
            if let Err(error) = store.put_setting(settings_keys::TMDB_API_KEY, key) {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        }
        if let Some(key) = body["tvdb_api_key"].as_str() {
            if let Err(error) = store.put_setting(settings_keys::TVDB_API_KEY, key) {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        }
        if let Some(language) = body["language"].as_str() {
            if let Err(error) = store.put_setting(settings_keys::METADATA_LANGUAGE, language) {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                );
            }
        }
        (
            store
                .get_setting(settings_keys::TMDB_API_KEY)
                .ok()
                .flatten(),
            store
                .get_setting(settings_keys::TVDB_API_KEY)
                .ok()
                .flatten(),
        )
    };
    ok(json!({
        "tmdb": {
            "configured": tmdb.as_deref().is_some_and(|k| !k.is_empty()),
            "api_key_hint": masked(tmdb),
        },
        "tvdb": {
            "configured": tvdb.as_deref().is_some_and(|k| !k.is_empty()),
            "api_key_hint": masked(tvdb),
        },
        "restart_required": true,
        "message": "元数据配置已保存，TVDB及全局语言变更将在服务重启后完整生效",
    }))
    .into_response()
}

/// Test the configured TMDB key with a live search.
pub(crate) async fn test_metadata(State(state): State<ApiState>) -> Response {
    let api_key = {
        let store = state.store.lock();
        store
            .get_setting(settings_keys::TMDB_API_KEY)
            .ok()
            .flatten()
            .filter(|k| !k.trim().is_empty())
    };
    let Some(api_key) = api_key else {
        return err(
            StatusCode::BAD_REQUEST,
            "metadata.unconfigured",
            "请先填写 TMDB API Key",
        );
    };
    // G06: 绕过本地 catalog 缓存，直连 TMDB 验证当前配置的 key 是否真实有效
    let test_url =
        format!("https://api.themoviedb.org/3/search/movie?query=Dune&api_key={api_key}");
    let res = tokio::task::spawn_blocking(move || {
        crate::http_agent::call(|agent| agent.get(&test_url).call())
    })
    .await;

    match res {
        Ok(Ok(resp)) => {
            let status = resp.status().as_u16();
            if status >= 400 {
                return ok(json!({
                    "ok": false,
                    "error": format!("TMDB API 响应错误 (HTTP {status})，请检查 API Key")
                }))
                .into_response();
            }
            let body_str = match resp.into_body().read_to_string() {
                Ok(s) => s,
                Err(e) => {
                    return ok(json!({
                        "ok": false,
                        "error": format!("读取响应失败: {e}")
                    }))
                    .into_response();
                }
            };
            let parsed: serde_json::Value = match serde_json::from_str(&body_str) {
                Ok(v) => v,
                Err(e) => {
                    return ok(json!({
                        "ok": false,
                        "error": format!("解析 TMDB 响应失败: {e}")
                    }))
                    .into_response();
                }
            };
            let results = parsed.get("results").and_then(|r| r.as_array());
            let count = results.map(|r| r.len()).unwrap_or(0);
            let sample = results
                .and_then(|r| r.first())
                .and_then(|item| item.get("title").or_else(|| item.get("name")))
                .and_then(|t| t.as_str())
                .unwrap_or("Dune")
                .to_string();
            ok(json!({
                "ok": true,
                "result_count": count,
                "sample": sample,
            }))
            .into_response()
        }
        Ok(Err(err)) => ok(json!({
            "ok": false,
            "error": format!("请求 TMDB 失败: {err}")
        }))
        .into_response(),
        Err(err) => ok(json!({
            "ok": false,
            "error": format!("执行测试任务失败: {err}")
        }))
        .into_response(),
    }
}

pub(crate) use super::proxy_settings::{diagnose_proxy, get_proxy, put_proxy, test_proxy};

pub(crate) use super::browser_settings::{
    get_browser_settings, get_obscura_status, post_obscura_start, post_obscura_stop,
    put_browser_settings,
};

/// Trigger CDP Cookie Sync across all supported PT sites.
/// Discovers cookies from Chrome, updates existing sites, and returns discovered candidate sites.
pub(crate) async fn sync_cdp_cookies(State(state): State<ApiState>) -> Response {
    let resolved_endpoint = {
        let config = state.browser.effective_config()
            .map_err(|e| (StatusCode::BAD_REQUEST, "cdp.disabled", e.to_string()));
        match config {
            Ok(c) => match c.resolve(None) {
                Ok(Some((endpoint, _))) => Ok(endpoint),
                Ok(None) => Err((
                    StatusCode::BAD_REQUEST,
                    "cdp.disabled",
                    "CDP 同步未开启，请先在系统设置中启用 CDP 开关".to_string(),
                )),
                Err(_) => Err((
                    StatusCode::BAD_REQUEST,
                    "cdp.no_endpoint",
                    "CDP 已开启但没有可用端点（未配置外部端点且受管浏览器未运行）".to_string(),
                )),
            },
            Err(e) => Err(e),
        }
    };

    let cdp_url = match resolved_endpoint {
        Ok(url) => url,
        Err((status, code, msg)) => return err(status, code, &msg),
    };

    // 1. Fetch raw cookies from Chrome CDP in a blocking thread
    let cookies_res =
        tokio::task::spawn_blocking(move || indexer::fetch_cookies_from_cdp(&cdp_url))
            .await
            .unwrap_or_else(|e| Err(indexer::IndexerError::Fetch(e.to_string())));

    let raw_cookies = match cookies_res {
        Ok(c) => c,
        Err(e) => return err(StatusCode::BAD_GATEWAY, "cdp.fetch_error", &e.to_string()),
    };

    // 2. Build list of all supported profile domains
    let profile_domains: Vec<(String, String)> = state
        .indexer
        .profile_ids()
        .into_iter()
        .filter_map(|pid| {
            let profile = state.indexer.profile(&pid)?;
            let domain = if let Some(base) = &profile.base_url {
                url_domain(base)
            } else if let Some(web_base) = &profile.web_base_url {
                url_domain(web_base)
            } else {
                url_domain(&profile.search.path)
            };
            domain.map(|d| (pid, d))
        })
        .collect();

    let matched = indexer::match_cookies_to_profiles(&raw_cookies, &profile_domains);

    // 3. Update existing sites in DB and find unconfigured candidates
    let store = state.store.lock();
    let existing_sites = store.list_sites().unwrap_or_default();
    let mut updated_count = 0;
    let mut candidate_sites = Vec::new();

    for item in &matched {
        if let Some(mut existing) = existing_sites
            .iter()
            .find(|s| s.profile_id == item.profile_id)
            .cloned()
        {
            // Update cookie
            existing.cookie = Some(item.cookie_header.clone());
            let _ = store.save_site(&existing);
            updated_count += 1;
        } else {
            // Candidate site that user has logged in on Chrome but not yet added
            candidate_sites.push(json!({
                "profile_id": item.profile_id,
                "domain": item.domain,
                "cookie_count": item.cookie_count,
            }));
        }
    }

    ok(json!({
        "total_cookies_scanned": raw_cookies.len(),
        "matched_sites": matched.len(),
        "updated_sites": updated_count,
        "candidate_sites": candidate_sites,
    }))
    .into_response()
}

fn url_domain(raw: &str) -> Option<String> {
    if let Ok(parsed) = url::Url::parse(raw) {
        return parsed
            .host_str()
            .map(|h| h.trim_start_matches('.').to_string());
    }
    // Fallback if raw is like "pterclub.net"
    if raw.contains('.') && !raw.starts_with('/') {
        return Some(raw.trim_start_matches('.').to_string());
    }
    None
}
