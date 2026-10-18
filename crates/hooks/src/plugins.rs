use domain::{Site, SiteId};
use indexer::Browser;

use crate::bus::{Bus, HookEvent, Step};
use crate::site_policy::{NexusPhpPolicy, SiteResponsePolicy, reject_challenge};

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("site not found")]
    NotFound,
    #[error("veto: {0}")]
    Veto(String),
    #[error("{0}")]
    Fetch(String),
    /// Browser failed before the target page was navigated (busy, endpoint,
    /// target creation, cookie injection). Nothing reached the site, so the
    /// caller may safely fall back to HTTP.
    #[error("browser pre-navigation: {0}")]
    BrowserPreNavigation(String),
}

impl PluginError {
    pub fn is_browser_pre_navigation(&self) -> bool {
        matches!(self, Self::BrowserPreNavigation(_))
    }
}

pub trait SiteCredentials {
    fn load(&self, id: SiteId) -> Result<Site, PluginError>;
    fn save(&self, site: &Site) -> Result<(), PluginError>;
}

pub trait HttpPost {
    fn post(
        &self,
        url: &str,
        body: &str,
        cookie: Option<&str>,
        proxy: Option<&str>,
    ) -> Result<String, PluginError>;

    /// Read an authenticated page. Login has no username/password submission contract.
    fn get(&self, _: &str, _: Option<&str>, _: Option<&str>) -> Result<String, PluginError> {
        Err(PluginError::Fetch(
            "HTTP 登录状态验证不可用；请更新 Cookie 或配置支持认证验证的 HTTP transport".into(),
        ))
    }
}

static DEFAULT_POLICY: NexusPhpPolicy = NexusPhpPolicy::new(None);

pub struct LoginPlugin<'a> {
    bus: &'a Bus,
    store: &'a dyn SiteCredentials,
    via: LoginVia<'a>,
    policy: &'a dyn SiteResponsePolicy,
    emit_hook: bool,
}

enum LoginVia<'a> {
    Http(&'a dyn HttpPost),
    Browser(&'a Browser),
}

impl<'a> LoginPlugin<'a> {
    pub fn http(bus: &'a Bus, store: &'a dyn SiteCredentials, http: &'a dyn HttpPost) -> Self {
        Self {
            bus,
            store,
            via: LoginVia::Http(http),
            policy: &DEFAULT_POLICY,
            emit_hook: true,
        }
    }

    pub fn browser(bus: &'a Bus, store: &'a dyn SiteCredentials, browser: &'a Browser) -> Self {
        Self {
            bus,
            store,
            via: LoginVia::Browser(browser),
            policy: &DEFAULT_POLICY,
            emit_hook: true,
        }
    }

    pub fn with_policy(mut self, policy: &'a dyn SiteResponsePolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Skip the Bus hook event: used when a browser attempt already emitted it
    /// and failed before navigation, so an HTTP fallback must not emit twice.
    pub fn without_hook_event(mut self) -> Self {
        self.emit_hook = false;
        self
    }

    pub fn login(&self, id: SiteId) -> Result<(), PluginError> {
        let via = match self.via {
            LoginVia::Http(_) => "http",
            LoginVia::Browser(_) => "browser",
        };
        tracing::info!(site_id = %id, via, "Site authentication verification started");
        let result = self.login_inner(id);
        match &result {
            Ok(_) => tracing::info!(site_id = %id, "Site authentication verified"),
            Err(error) => {
                tracing::error!(site_id = %id, %error, "Site authentication verification failed")
            }
        }
        result
    }

    fn login_inner(&self, id: SiteId) -> Result<(), PluginError> {
        if self.emit_hook {
            self.bus.emit(&HookEvent { step: Step::Login })?;
        }
        let mut site = self.store.load(id)?;
        require_cookie(&site)?;
        // No credentials can be submitted: verify the supplied session, not an empty login form.
        let url = format!("{}index.php", site_base(&site));
        let (body, cookie_update) = match self.via {
            LoginVia::Http(http) => {
                let response = http.get(&url, site.cookie.as_deref(), site.proxy.as_deref())?;
                let body = response_body(&response).to_string();
                let cookie = cookies_from_response(&response);
                (body, cookie)
            }
            LoginVia::Browser(browser) => {
                let page = browser_render(browser, &site, &url)?;
                (page.html, page.cookie_header)
            }
        };
        reject_challenge(&body)?;
        self.policy.authenticated(&site, &body)?;
        if let Some(cookie) = cookie_update.as_deref() {
            site.cookie = Some(merge_cookie_string(site.cookie.as_deref(), cookie));
            self.store.save(&site)?;
        }
        Ok(())
    }
}

fn browser_render(
    browser: &Browser,
    site: &Site,
    url: &str,
) -> Result<indexer::RenderedPage, PluginError> {
    let request = indexer::FetchRequest {
        key: format!("maintenance:{}", site.id),
        url: url.to_string(),
        method: indexer::FetchMethod::Get,
        body: None,
        cookie: site.cookie.clone(),
        api_key: None,
        proxy: site.proxy.clone(),
        cdp_url: site.cdp_url.clone(),
    };
    browser
        .render(&request)
        .map_err(|err| match err {
            indexer::IndexerError::BrowserSetup(_) | indexer::IndexerError::BrowserBusy => {
                PluginError::BrowserPreNavigation(err.to_string())
            }
            other => PluginError::Fetch(other.to_string()),
        })
}

fn require_cookie(site: &Site) -> Result<(), PluginError> {
    if site
        .cookie
        .as_deref()
        .is_some_and(|cookie| !cookie.trim().is_empty())
    {
        Ok(())
    } else {
        tracing::error!(site_id = %site.id, capability = "credential_submission", "Site maintenance requires an existing Cookie; automatic login is unsupported");
        Err(PluginError::Fetch(
            "自动登录不支持提交凭据；请人工登录并配置有效 Cookie 后重试".into(),
        ))
    }
}

fn site_base(site: &Site) -> String {
    let mut base = site.url.clone();
    if !base.ends_with('/') {
        base.push('/');
    }
    base
}

fn response_body(mut response: &str) -> &str {
    while response.starts_with("Set-Cookie: ") {
        response = response
            .split_once('\n')
            .map(|(_, body)| body)
            .unwrap_or("");
    }
    response
}

fn cookies_from_response(response: &str) -> Option<String> {
    let cookies: Vec<&str> = response
        .lines()
        .take_while(|line| line.starts_with("Set-Cookie: "))
        .filter_map(|line| line.strip_prefix("Set-Cookie: "))
        .filter_map(|value| value.split(';').next())
        .filter(|value| {
            value
                .split_once('=')
                .is_some_and(|(key, _)| !key.trim().is_empty())
        })
        .collect();
    (!cookies.is_empty()).then(|| cookies.join("; "))
}

fn merge_cookie_string(old_cookie: Option<&str>, new_cookie: &str) -> String {
    let mut map = std::collections::BTreeMap::new();
    for part in old_cookie
        .unwrap_or_default()
        .split(';')
        .chain(new_cookie.split(';'))
    {
        if let Some((key, value)) = part.trim().split_once('=') {
            map.insert(key.trim(), value.trim());
        }
    }
    map.iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

enum CheckInVia<'a> {
    Http(&'a dyn HttpPost),
    Browser(&'a Browser),
}

pub struct CheckInPlugin<'a> {
    bus: &'a Bus,
    store: &'a dyn SiteCredentials,
    via: Option<CheckInVia<'a>>,
    policy: &'a dyn SiteResponsePolicy,
    emit_hook: bool,
}

impl<'a> CheckInPlugin<'a> {
    pub fn new(bus: &'a Bus, store: &'a dyn SiteCredentials) -> Self {
        Self {
            bus,
            store,
            via: None,
            policy: &DEFAULT_POLICY,
            emit_hook: true,
        }
    }

    pub fn http(bus: &'a Bus, store: &'a dyn SiteCredentials, http: &'a dyn HttpPost) -> Self {
        Self {
            bus,
            store,
            via: Some(CheckInVia::Http(http)),
            policy: &DEFAULT_POLICY,
            emit_hook: true,
        }
    }

    pub fn browser(bus: &'a Bus, store: &'a dyn SiteCredentials, browser: &'a Browser) -> Self {
        Self {
            bus,
            store,
            via: Some(CheckInVia::Browser(browser)),
            policy: &DEFAULT_POLICY,
            emit_hook: true,
        }
    }

    pub fn with_policy(mut self, policy: &'a dyn SiteResponsePolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Skip the Bus hook event: used when a browser attempt already emitted it
    /// and failed before navigation, so an HTTP fallback must not emit twice.
    pub fn without_hook_event(mut self) -> Self {
        self.emit_hook = false;
        self
    }

    pub fn check_in(&self, id: SiteId) -> Result<(), PluginError> {
        tracing::info!(site_id = %id, "Site Check-in started");
        let result = self.check_in_inner(id);
        match &result {
            Ok(_) => tracing::info!(site_id = %id, "Site Check-in verified"),
            Err(error) => tracing::error!(site_id = %id, %error, "Site Check-in failed"),
        }
        result
    }

    fn check_in_inner(&self, id: SiteId) -> Result<(), PluginError> {
        let mut site = self.store.load(id)?;
        if self.emit_hook {
            self.bus.emit(&HookEvent {
                step: Step::CheckIn,
            })?;
        }
        let via = self.via.as_ref().ok_or_else(|| {
            PluginError::Fetch("Check-in 不支持仅分发 Hook；请配置 transport".into())
        })?;
        require_cookie(&site)?;
        let url = format!("{}attendance.php", site_base(&site));
        let (body, cookie_update) = match via {
            CheckInVia::Http(http) => {
                let response = http.post(&url, "", site.cookie.as_deref(), site.proxy.as_deref())?;
                let body = response_body(&response).to_string();
                let cookie = cookies_from_response(&response);
                (body, cookie)
            }
            CheckInVia::Browser(browser) => {
                let page = browser_render(browser, &site, &url)?;
                (page.html, page.cookie_header)
            }
        };
        reject_challenge(&body)?;
        let outcome = self.policy.check_in(&site, &body)?;
        tracing::debug!(site_id = %id, ?outcome, "Site attendance business outcome verified");
        if let Some(cookie) = cookie_update.as_deref() {
            site.cookie = Some(merge_cookie_string(site.cookie.as_deref(), cookie));
            self.store.save(&site)?;
        }
        Ok(())
    }

    pub fn tick(&self, _elapsed: std::time::Duration, id: SiteId) -> Result<(), PluginError> {
        self.check_in(id)
    }
}

pub fn keep_site_alive(
    bus: &Bus,
    store: &dyn SiteCredentials,
    http: &dyn HttpPost,
    id: SiteId,
) -> Result<(), PluginError> {
    let site = store.load(id)?;
    require_cookie(&site)?;
    CheckInPlugin::http(bus, store, http).check_in(id)
}
