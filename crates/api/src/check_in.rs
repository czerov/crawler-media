use domain::{Site, SiteId};
use hooks::{Bus, CheckInPlugin, HttpPost, NexusPhpPolicy, PluginError, SiteCredentials};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::Store;
use crate::management::ApiState;

pub fn run(state: &ApiState) -> Result<(), String> {
    let sites = state
        .store
        .lock()
        .list_enabled_sites()
        .map_err(|e| e.to_string())?;
    tracing::info!(
        "【站点签到】开始执行周期性签到与保活任务，已启用站点数量: {}",
        sites.len()
    );
    let bus = Bus::new();
    let creds = StoreSites(&state.store);
    let http = AttendanceHttp;
    let mut failures = Vec::new();
    for site in sites {
        // 判断站点类型：纯 API 架构站点（如新版 M-Team）无 attendance.php，无需也不支持传统打卡保活
        if let Some(profile) = state.indexer.profile(&site.profile_id) {
            if profile.framework == indexer::Framework::Api {
                tracing::info!(site = %site.name, site_id = %site.id, "【站点签到】API 令牌架构站点，跳过页面打卡签到");
                continue;
            }
        }

        tracing::info!(site = %site.name, site_id = %site.id, "【站点签到】正在执行站点签到/保活请求");
        let result = crate::site_maintenance::perform_check_in(state, &site, &bus, &creds);
        match result {
            Ok(_) => {
                tracing::info!(site = %site.name, site_id = %site.id, "【站点签到】签到/保活成功");
            }
            Err(error) => {
                tracing::error!(site = %site.name, site_id = %site.id, error = %error, "【站点签到】签到/保活失败");
                let title = format!("站点签到失败：{}", site.name);
                let content = error.to_string();
                crate::worker::notify_public(state, &title, &content);
                failures.push(format!("{}: {error}", site.name));
            }
        }
    }
    tracing::info!("【站点签到】本轮签到检查完成，正在进行刷流配额检查");
    boost_check(state)?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("站点签到失败: {}", failures.join("; ")))
    }
}

/// 刷流检查：每个启用 boost 且有 budget 的站点，若当前上传量达到预算
/// 则在统计里标 upper_limit_reached（按上游刷流语义，后续可暂停）。
fn boost_check(state: &ApiState) -> Result<(), String> {
    let category = state
        .store
        .lock()
        .default_downloader()
        .ok()
        .flatten()
        .and_then(|d| d.category)
        .unwrap_or_default();
    let uploaded = state
        .downloader
        .uploaded_by_category(&category)
        .unwrap_or(0);
    let store = state.store.lock();
    let sites = store.list_enabled_sites().map_err(|e| e.to_string())?;
    for site in sites {
        let key = format!("boost.{}", site.id);
        let Some(raw) = store.get_setting(&key).ok().flatten() else {
            continue;
        };
        let Ok(config) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if config["enabled"] != true {
            continue;
        }
        let budget = config["budget_bytes"].as_u64().unwrap_or(0);
        let reached = budget > 0 && uploaded >= budget;
        let mut next = config.clone();
        next["upper_limit_reached"] = json!(reached);
        let _ = store.put_setting(&key, &next.to_string());
    }
    Ok(())
}

pub(crate) fn attendance_http() -> AttendanceHttp {
    AttendanceHttp
}

pub(crate) fn store_sites(store: &Mutex<Store>) -> StoreSites<'_> {
    StoreSites(store)
}

pub(crate) struct AttendanceHttp;

impl HttpPost for AttendanceHttp {
    fn post(
        &self,
        url: &str,
        body: &str,
        cookie: Option<&str>,
        proxy: Option<&str>,
    ) -> Result<String, PluginError> {
        attendance_request(url, Some(body), cookie, proxy)
    }

    fn get(
        &self,
        url: &str,
        cookie: Option<&str>,
        proxy: Option<&str>,
    ) -> Result<String, PluginError> {
        attendance_request(url, None, cookie, proxy)
    }
}

pub(crate) fn site_policy(state: &ApiState, site: &Site) -> Result<NexusPhpPolicy, PluginError> {
    let profile = state.indexer.profile(&site.profile_id)
        .ok_or_else(|| {
            tracing::error!(site_id = %site.id, profile_id = %site.profile_id, "Site maintenance profile unavailable");
            PluginError::Fetch("Site profile 不可用；请选择支持认证验证的模板".into())
        })?;
    if profile.framework != indexer::Framework::Nexusphp {
        tracing::error!(site_id = %site.id, profile_id = %site.profile_id, "Site page maintenance unsupported for API profile");
        return Err(PluginError::Fetch(
            "API Site 不支持页面 Login/Check-in；请配置有效 API key".into(),
        ));
    }
    Ok(NexusPhpPolicy::new(profile.login_success_css.clone()))
}

fn attendance_request(
    url: &str,
    body: Option<&str>,
    cookie: Option<&str>,
    proxy: Option<&str>,
) -> Result<String, PluginError> {
    let parsed = url::Url::parse(url).map_err(|error| PluginError::Fetch(error.to_string()))?;
    let host = parsed.host_str().unwrap_or_default();
    if host.ends_with(".example")
        || host.ends_with(".invalid")
        || host.ends_with(".test")
        || ["example.com", "example.org", "example.net"].contains(&host)
    {
        tracing::error!(
            site_host = host,
            "Site maintenance rejected placeholder URL"
        );
        return Err(PluginError::Fetch(
            "Site URL 是占位地址；请配置实际站点地址与有效 Cookie".into(),
        ));
    }
    let mut config =
        ureq::config::Config::builder().timeout_global(Some(std::time::Duration::from_secs(10)));
    if let Some(proxy) = proxy.filter(|proxy| !proxy.trim().is_empty()) {
        let proxy = ureq::Proxy::new(proxy)
            .map_err(|error| PluginError::Fetch(format!("invalid Site proxy: {error}")))?;
        config = config.proxy(Some(proxy));
    }
    let method = if body.is_some() { "POST" } else { "GET" };
    let mut request = ureq::http::Request::builder().method(method).uri(url)
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36");
    if body.is_some() {
        request = request.header("Content-Type", "application/x-www-form-urlencoded");
    }
    if let Some(cookie) = cookie {
        request = request.header("Cookie", cookie);
    }
    let request = request
        .body(body.unwrap_or_default())
        .map_err(|error| PluginError::Fetch(error.to_string()))?;
    let started = std::time::Instant::now();
    let response = ureq::Agent::new_with_config(config.build()).run(request).map_err(|error| {
        tracing::error!(site_host = host, method, %error, "Site maintenance HTTP request failed");
        PluginError::Fetch(error.to_string())
    })?;
    tracing::debug!(
        site_host = host,
        method,
        elapsed_ms = started.elapsed().as_millis(),
        status = response.status().as_u16(),
        "Site maintenance page fetched; business outcome still requires validation"
    );
    let cookies: Vec<String> = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(|value| format!("Set-Cookie: {value}\n"))
        .collect();
    let text = response.into_body().read_to_string().map_err(|error| {
        tracing::error!(site_host = host, method, %error, "Site maintenance response read failed");
        PluginError::Fetch(error.to_string())
    })?;
    Ok(format!("{}{text}", cookies.concat()))
}

pub(crate) struct StoreSites<'a>(&'a Mutex<Store>);

impl SiteCredentials for StoreSites<'_> {
    fn load(&self, id: SiteId) -> Result<Site, PluginError> {
        self.0
            .lock()
            .get_site(id)
            .map_err(|e| PluginError::Fetch(e.to_string()))?
            .ok_or(PluginError::NotFound)
    }

    fn save(&self, site: &Site) -> Result<(), PluginError> {
        if self
            .0
            .lock()
            .save_site(site)
            .map_err(|e| PluginError::Fetch(e.to_string()))?
        {
            Ok(())
        } else {
            Err(PluginError::NotFound)
        }
    }
}
