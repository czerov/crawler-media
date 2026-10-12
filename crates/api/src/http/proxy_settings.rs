use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::http::{err, ok};
use crate::management::ApiState;
use crate::settings_keys;

pub(crate) fn mask_proxy_url(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        if let Some((creds, host)) = rest.split_once('@') {
            if let Some((user, _pass)) = creds.split_once(':') {
                return format!("{scheme}://{user}:***@{host}");
            }
            return format!("{scheme}://{creds}@{host}");
        }
    }
    url.to_string()
}

/// GET /settings/proxy — Read current metadata proxy configuration.
pub(crate) async fn get_proxy(State(state): State<ApiState>) -> Response {
    let store = state.store.lock();
    let proxy_url = store
        .get_setting(settings_keys::PROXY_METADATA)
        .ok()
        .flatten()
        .unwrap_or_default();
    let username = store
        .get_setting(settings_keys::PROXY_USERNAME)
        .ok()
        .flatten()
        .unwrap_or_default();
    let has_password = store
        .get_setting(settings_keys::PROXY_PASSWORD)
        .ok()
        .flatten()
        .is_some_and(|p| !p.is_empty());
    let douban_bypass = store
        .get_setting(settings_keys::PROXY_DOUBAN_BYPASS)
        .ok()
        .flatten()
        .and_then(|v| crate::config::parse_bool(&v))
        .unwrap_or(true);
    let allowed_domains = store
        .get_setting(settings_keys::PROXY_ALLOWED_DOMAINS)
        .ok()
        .flatten()
        .unwrap_or_default();
    // 非空才算 override，且与 http_agent 用同一组 env 变量（空串不生效）。
    let env_override = crate::config::proxy_env(&crate::config::ProcessEnv)
        .url
        .is_some();

    let active_masked = crate::http_agent::current_proxy().map(|p| mask_proxy_url(&p));

    ok(json!({
        "proxy_url": proxy_url,
        "username": username,
        "has_password": has_password,
        "douban_bypass": douban_bypass,
        "allowed_domains": allowed_domains,
        "active_proxy": active_masked,
        "env_override": env_override,
    }))
    .into_response()
}

/// PUT /settings/proxy — Save metadata proxy configuration and reload immediately.
///
/// Fields pinned by the environment are rejected with 409: the startup seed
/// would overwrite them on the next restart.
pub(crate) async fn put_proxy(State(state): State<ApiState>, Json(body): Json<Value>) -> Response {
    for (field, key) in [
        ("proxy_url", settings_keys::PROXY_METADATA),
        ("username", settings_keys::PROXY_USERNAME),
        ("password", settings_keys::PROXY_PASSWORD),
    ] {
        if body[field].as_str().is_some() {
            if let Some(message) = crate::config::env_pinned_message(key) {
                return err(StatusCode::CONFLICT, "settings.env_pinned", &message);
            }
        }
    }
    let store = state.store.lock();
    if let Some(proxy_url) = body["proxy_url"].as_str() {
        if let Err(error) = store.put_setting(settings_keys::PROXY_METADATA, proxy_url.trim()) {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    }
    if let Some(username) = body["username"].as_str() {
        if let Err(error) = store.put_setting(settings_keys::PROXY_USERNAME, username.trim()) {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    }
    if let Some(password) = body["password"].as_str() {
        if let Err(error) = store.put_setting(settings_keys::PROXY_PASSWORD, password.trim()) {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    }
    if let Some(douban_bypass) = body["douban_bypass"].as_bool() {
        let val_str = if douban_bypass { "true" } else { "false" };
        if let Err(error) = store.put_setting(settings_keys::PROXY_DOUBAN_BYPASS, val_str) {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    }
    if let Some(allowed_domains) = body["allowed_domains"].as_str() {
        if let Err(error) =
            store.put_setting(settings_keys::PROXY_ALLOWED_DOMAINS, allowed_domains.trim())
        {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    }
    crate::http_agent::sync_from_store(&store);

    let proxy_url = store
        .get_setting(settings_keys::PROXY_METADATA)
        .ok()
        .flatten()
        .unwrap_or_default();
    let username = store
        .get_setting(settings_keys::PROXY_USERNAME)
        .ok()
        .flatten()
        .unwrap_or_default();
    let has_password = store
        .get_setting(settings_keys::PROXY_PASSWORD)
        .ok()
        .flatten()
        .is_some_and(|p| !p.is_empty());
    let douban_bypass = store
        .get_setting(settings_keys::PROXY_DOUBAN_BYPASS)
        .ok()
        .flatten()
        .and_then(|v| crate::config::parse_bool(&v))
        .unwrap_or(true);

    let active_masked = crate::http_agent::current_proxy().map(|p| mask_proxy_url(&p));

    ok(json!({
        "proxy_url": proxy_url,
        "username": username,
        "has_password": has_password,
        "douban_bypass": douban_bypass,
        "active_proxy": active_masked,
    }))
    .into_response()
}

/// POST /settings/proxy/test — Probe connection through specified proxy.
pub(crate) async fn test_proxy(
    State(_state): State<ApiState>,
    Json(body): Json<Value>,
) -> Response {
    let proxy_url = body["proxy_url"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let username = body["username"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let password = body["password"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let target = body["target"]
        .as_str()
        .unwrap_or("https://api.themoviedb.org");

    let formatted_proxy =
        proxy_url.map(|url| crate::http_agent::format_proxy_url(url, username, password));

    let started = std::time::Instant::now();
    let proxy_for_block = formatted_proxy;
    let target_for_block = target.to_string();

    let probe_res = tokio::task::spawn_blocking(move || {
        crate::http_agent::call_with_proxy(proxy_for_block.as_deref(), |agent| {
            agent
                .get(&target_for_block)
                .header("User-Agent", "crawler-media-probe/1.0")
                .call()
        })
    })
    .await;

    let elapsed_ms = started.elapsed().as_millis() as i64;

    match probe_res {
        Ok(Ok(_)) => ok(json!({
            "ok": true,
            "latency_ms": elapsed_ms,
            "error": Value::Null,
        }))
        .into_response(),
        Ok(Err(err)) => ok(json!({
            "ok": false,
            "latency_ms": elapsed_ms,
            "error": err.to_string(),
        }))
        .into_response(),
        Err(join_err) => ok(json!({
            "ok": false,
            "latency_ms": elapsed_ms,
            "error": format!("探测异常: {join_err}"),
        }))
        .into_response(),
    }
}

/// POST /settings/proxy/diagnose — Check routing and connectivity for all major metadata domains.
pub(crate) async fn diagnose_proxy(State(state): State<ApiState>) -> Response {
    let (active_proxy, douban_bypass, tmdb_key) = {
        let store = state.store.lock();
        let key = store
            .get_setting(settings_keys::TMDB_API_KEY)
            .ok()
            .flatten();
        (
            crate::http_agent::current_proxy(),
            crate::http_agent::douban_bypass(),
            key,
        )
    };

    let targets = build_diagnose_targets(tmdb_key.as_deref());
    let mut set = tokio::task::JoinSet::new();

    for target in targets {
        let proxy = if target.bypassable_for_douban && douban_bypass {
            None
        } else {
            active_proxy.clone()
        };
        let is_proxied = proxy.is_some();
        let masked_proxy = proxy.as_deref().map(crate::http_agent::mask_proxy_url);

        let route_label = if let Some(p) = masked_proxy {
            format!("代理 ({p})")
        } else if target.bypassable_for_douban && douban_bypass && active_proxy.is_some() {
            "直连 (已绕过代理)".to_string()
        } else {
            "直连 (未配置代理)".to_string()
        };

        set.spawn(async move {
            run_single_probe(target, proxy, is_proxied, route_label).await
        });
    }

    let mut results = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(val) = res {
            results.push(val);
        }
    }

    ok(json!({
        "active_proxy": active_proxy.as_deref().map(crate::http_agent::mask_proxy_url),
        "douban_bypass": douban_bypass,
        "items": results,
    }))
    .into_response()
}

enum ProbeMethod {
    Get {
        ua: &'static str,
        referer: Option<&'static str>,
    },
    PostJson {
        ua: &'static str,
        body: &'static str,
    },
}

struct TargetProbe {
    name: &'static str,
    display_url: &'static str,
    probe_url: String,
    method: ProbeMethod,
    bypassable_for_douban: bool,
}

fn build_diagnose_targets(tmdb_key: Option<&str>) -> Vec<TargetProbe> {
    let tmdb_probe_url = match tmdb_key.filter(|k| !k.is_empty()) {
        Some(k) => format!("https://api.themoviedb.org/3/configuration?api_key={k}"),
        None => "https://api.themoviedb.org/3/configuration".to_string(),
    };

    vec![
        TargetProbe {
            name: "TMDB (条目数据)",
            display_url: "https://api.themoviedb.org",
            probe_url: tmdb_probe_url,
            method: ProbeMethod::Get {
                ua: "crawler-media/0.1",
                referer: None,
            },
            bypassable_for_douban: false,
        },
        TargetProbe {
            name: "TMDB (海报封面 CDN)",
            display_url: "https://image.tmdb.org",
            probe_url: "https://image.tmdb.org/t/p/w92/7WsyChQLEftFiDOVTGkv3hFpyyt.jpg".to_string(),
            method: ProbeMethod::Get {
                ua: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)",
                referer: None,
            },
            bypassable_for_douban: false,
        },
        TargetProbe {
            name: "Bangumi (动漫番组)",
            display_url: "https://api.bgm.tv",
            probe_url: "https://api.bgm.tv/calendar".to_string(),
            method: ProbeMethod::Get {
                ua: "crawler-media/0.1 (https://github.com/jianxcao/crawler-media)",
                referer: None,
            },
            bypassable_for_douban: false,
        },
        TargetProbe {
            name: "AniList (海外番剧)",
            display_url: "https://graphql.anilist.co",
            probe_url: "https://graphql.anilist.co".to_string(),
            method: ProbeMethod::PostJson {
                ua: "crawler-media/0.1",
                body: r#"{"query":"{ Page(page: 1, perPage: 1) { media { id } } }"}"#,
            },
            bypassable_for_douban: false,
        },
        TargetProbe {
            name: "豆瓣电影 (华语条目)",
            display_url: "https://movie.douban.com",
            probe_url: "https://movie.douban.com".to_string(),
            method: ProbeMethod::Get {
                ua: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
                referer: Some("https://movie.douban.com/"),
            },
            bypassable_for_douban: true,
        },
    ]
}

async fn run_single_probe(
    target: TargetProbe,
    proxy: Option<String>,
    is_proxied: bool,
    route_label: String,
) -> Value {
    let started = std::time::Instant::now();
    let probe_url = target.probe_url.to_string();
    let proxy_for_block = proxy;

    let probe_res = tokio::task::spawn_blocking(move || {
        crate::http_agent::call_with_proxy(proxy_for_block.as_deref(), |agent| match target.method {
            ProbeMethod::Get { ua, referer } => {
                let mut req = agent.get(&probe_url).header("User-Agent", ua);
                if let Some(ref_val) = referer {
                    req = req.header("Referer", ref_val);
                }
                req.call()
            }
            ProbeMethod::PostJson { ua, body } => agent
                .post(&probe_url)
                .header("User-Agent", ua)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json")
                .send(body),
        })
    })
    .await;

    let latency_ms = started.elapsed().as_millis() as i64;
    let (ok, error) = match probe_res {
        Ok(Ok(response)) => {
            let code = response.status().as_u16();
            if code < 400 {
                (true, None)
            } else if code == 401 {
                (true, Some("服务可达 (401 未配置Key)".to_string()))
            } else {
                (false, Some(format!("HTTP 状态码: {code}")))
            }
        }
        Ok(Err(err)) => (false, Some(err.to_string())),
        Err(join_err) => (false, Some(format!("调度异常: {join_err}"))),
    };

    json!({
        "name": target.name,
        "url": target.display_url,
        "is_proxied": is_proxied,
        "route_label": route_label,
        "ok": ok,
        "latency_ms": latency_ms,
        "error": error,
    })
}
