use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::IndexerError;
use crate::fetch::{FetchRequest, Fetcher};

#[path = "browser_slots.rs"]
mod browser_slots;
pub use browser_slots::BrowserSlots;

pub trait PageSession: Send + Sync {
    fn goto(&self, url: &str) -> Result<(), IndexerError>;
    fn set_site_cookies(&self, page_url: &str, header: &str) -> Result<(), IndexerError> {
        let _ = (page_url, header);
        Ok(())
    }
    fn content(&self) -> Result<String, IndexerError>;
    fn cookie_header(&self, page_url: &str) -> Result<String, IndexerError> {
        let _ = page_url;
        Err(IndexerError::BrowserSetup("Browser cookie readback unsupported".into()))
    }
    fn set_deadline(&self, deadline: Instant) {
        let _ = deadline;
    }
    #[deprecated(note = "use set_site_cookies")]
    fn set_cookie_header(&self, _header: &str) -> Result<(), IndexerError> {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedPage {
    pub html: String,
    pub cookie_header: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserRoute {
    Site,
    Instance,
    Managed,
}

type Opener = Arc<dyn Fn(Option<&str>) -> Result<Arc<dyn PageSession>, IndexerError> + Send + Sync>;
type ConfigProvider = Arc<dyn Fn() -> Result<BrowserConfig, IndexerError> + Send + Sync>;

/// Configuration for browser endpoint resolution.
#[derive(Clone, Debug, Default)]
pub struct BrowserConfig {
    pub enabled: bool,
    pub endpoint: Option<String>,
    pub managed_endpoint: Option<String>,
}

impl BrowserConfig {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn enabled_for_test() -> Self {
        Self {
            enabled: true,
            endpoint: None,
            managed_endpoint: None,
        }
    }

    pub fn validate_endpoint(endpoint: &str) -> Result<(), IndexerError> {
        let uri = endpoint
            .parse::<tungstenite::http::Uri>()
            .map_err(|_| IndexerError::BrowserSetup("Browser requires a valid HTTP CDP discovery URL; only http scheme is supported without credentials or query".into()))?;
        if uri.scheme_str() != Some("http") {
            return Err(IndexerError::BrowserSetup("仅支持 http CDP discovery（建议仅回环或受信内网）".into()));
        }
        if uri.host().is_none_or(|host| host.is_empty())
            || uri.authority().is_some_and(|a| a.as_str().contains('@'))
            || uri.path_and_query().is_some_and(|p| p.query().is_some())
        {
            return Err(IndexerError::BrowserSetup("Browser requires a valid HTTP CDP discovery URL; empty, credentials, query and ws/wss endpoints are unsupported".into()));
        }
        Ok(())
    }

    pub fn resolve(
        &self,
        site: Option<&str>,
    ) -> Result<Option<(String, BrowserRoute)>, IndexerError> {
        if !self.enabled {
            return Ok(None);
        }
        if let Some(site_url) = site {
            let trimmed = site_url.trim();
            if trimmed.is_empty() {
                return Err(IndexerError::BrowserSetup("Site CDP URL cannot be empty".into()));
            }
            Self::validate_endpoint(trimmed)?;
            return Ok(Some((trimmed.to_string(), BrowserRoute::Site)));
        }
        if let Some(endpoint) = &self.endpoint {
            let trimmed = endpoint.trim();
            if !trimmed.is_empty() {
                Self::validate_endpoint(trimmed)?;
                return Ok(Some((trimmed.to_string(), BrowserRoute::Instance)));
            }
        }
        if let Some(managed) = &self.managed_endpoint {
            let trimmed = managed.trim();
            if !trimmed.is_empty() {
                Self::validate_endpoint(trimmed)?;
                return Ok(Some((trimmed.to_string(), BrowserRoute::Managed)));
            }
        }
        Err(IndexerError::BrowserSetup("CDP 已开启但没有可用端点".into()))
    }
}

#[derive(Clone)]
pub struct Browser {
    config: BrowserConfig,
    managed_endpoint: Arc<parking_lot::RwLock<Option<String>>>,
    provider: Option<ConfigProvider>,
    opener: Option<Opener>,
    slots: Arc<BrowserSlots>,
    max_sessions: usize,
    render_deadline: Duration,
    queue_cap: Duration,
    min_work: Duration,
    target_close_cap: Duration,
    /// Endpoints found unable to isolate sessions (see `cdp_page::INCOMPATIBLE_PREFIX`).
    incompatible: Arc<parking_lot::Mutex<std::collections::BTreeMap<String, String>>>,
}

impl Browser {
    pub fn new(config: BrowserConfig) -> Self {
        let max_sessions = 3;
        Self {
            config,
            managed_endpoint: Arc::new(parking_lot::RwLock::new(None)),
            provider: None,
            opener: None,
            slots: BrowserSlots::new(max_sessions),
            max_sessions,
            render_deadline: Duration::from_secs(30),
            queue_cap: Duration::from_secs(15),
            min_work: Duration::from_secs(10),
            target_close_cap: Duration::from_secs(2),
            incompatible: Arc::new(parking_lot::Mutex::new(Default::default())),
        }
    }

    pub fn with_opener<F>(config: BrowserConfig, opener: F) -> Self
    where
        F: Fn(Option<&str>) -> Result<Arc<dyn PageSession>, IndexerError> + Send + Sync + 'static,
    {
        let mut b = Self::new(config);
        b.opener = Some(Arc::new(opener));
        b
    }

    pub fn with_config_provider<F>(mut self, provider: F) -> Self
    where
        F: Fn() -> Result<BrowserConfig, IndexerError> + Send + Sync + 'static,
    {
        self.provider = Some(Arc::new(provider));
        self
    }

    pub fn with_max_sessions(mut self, n: usize) -> Self {
        self.max_sessions = n.clamp(1, 8);
        self.slots = BrowserSlots::new(self.max_sessions);
        self
    }

    pub fn with_render_deadline(mut self, d: Duration) -> Self {
        self.render_deadline = d;
        self
    }

    pub fn with_queue_cap(mut self, d: Duration) -> Self {
        self.queue_cap = d;
        self
    }

    pub fn with_min_work(mut self, d: Duration) -> Self {
        self.min_work = d;
        self
    }

    pub fn with_target_close_cap(mut self, d: Duration) -> Self {
        self.target_close_cap = d;
        self
    }

    pub fn validate_timing(&self) -> Result<(), IndexerError> {
        if self.min_work + Duration::from_secs(1) > self.render_deadline {
            return Err(IndexerError::BrowserSetup(
                "min_work + 1s must be <= render_deadline".into(),
            ));
        }
        if self.queue_cap + self.min_work > self.render_deadline {
            return Err(IndexerError::BrowserSetup(
                "queue_cap must be <= render_deadline - min_work".into(),
            ));
        }
        Ok(())
    }

    /// Endpoints marked incompatible (endpoint -> reason).
    pub fn incompatible_endpoints(&self) -> std::collections::BTreeMap<String, String> {
        self.incompatible.lock().clone()
    }

    /// Forget incompatibility marks (e.g. after the user changes endpoints).
    pub fn clear_incompatible(&self) {
        self.incompatible.lock().clear();
    }

    pub fn set_managed_endpoint(&self, endpoint: Option<String>) {
        *self.managed_endpoint.write() = endpoint;
    }

    pub fn effective_config(&self) -> Result<BrowserConfig, IndexerError> {
        let mut config = match &self.provider {
            Some(provider) => provider()?,
            None => self.config.clone(),
        };
        if config.managed_endpoint.is_none() {
            config.managed_endpoint = self.managed_endpoint.read().clone();
        }
        Ok(config)
    }

    pub fn cdp_enabled(&self) -> Result<bool, IndexerError> {
        self.effective_config().map(|c| c.enabled)
    }

    pub fn config(&self) -> &BrowserConfig {
        &self.config
    }

    fn open_until(
        &self,
        cdp_url: Option<&str>,
        deadline: Instant,
    ) -> Result<Arc<dyn PageSession>, IndexerError> {
        let config = self.effective_config()?;
        let (endpoint, route) = match config.resolve(cdp_url)? {
            Some((ep, r)) => (ep, r),
            None => return Err(IndexerError::BrowserSetup("CDP disabled".into())),
        };
        if let Some(reason) = self.incompatible.lock().get(&endpoint).cloned() {
            return Err(IndexerError::BrowserSetup(reason));
        }
        tracing::debug!(?route, endpoint = %endpoint, "Opening external Browser session");
        let session = match &self.opener {
            Some(opener) => opener(Some(&endpoint)),
            None => crate::cdp_page::open_cdp_session_until(&endpoint, deadline, self.target_close_cap)
                .map(|session| Arc::new(session) as Arc<dyn PageSession>),
        };
        session.inspect_err(|error| {
            if let IndexerError::BrowserSetup(message) = error {
                if message.starts_with(crate::cdp_page::INCOMPATIBLE_PREFIX) {
                    tracing::error!(endpoint = %endpoint, %message, "Marking Browser endpoint incompatible");
                    self.incompatible.lock().insert(endpoint.clone(), message.clone());
                }
            }
            tracing::error!(%error, ?route, "Browser session open failed");
        })
    }

    pub fn render(&self, request: &FetchRequest) -> Result<RenderedPage, IndexerError> {
        self.validate_timing()?;
        if request.proxy.as_deref().is_some_and(|p| !p.trim().is_empty()) {
            return Err(IndexerError::BrowserSetup(
                "Browser 不支持站点代理（proxy 与 browser 互斥）".into(),
            ));
        }
        let deadline = Instant::now() + self.render_deadline;
        let uri: tungstenite::http::Uri = request
            .url
            .parse()
            .map_err(|e| IndexerError::BrowserSetup(format!("invalid request URL: {e}")))?;
        let host = uri
            .host()
            .ok_or_else(|| IndexerError::BrowserSetup("missing host in request URL".into()))?;
        let group = crate::browser_host::try_group_key(host)?;

        let slot = self
            .slots
            .acquire(&group, deadline, self.queue_cap, self.min_work)?;
        let page = self
            .open_until(request.cdp_url.as_deref(), deadline)
            .map_err(crate::cdp_page::into_setup)?;
        page.set_deadline(deadline);

        let result = (|| {
            if let Some(cookie) = &request.cookie {
                page.set_site_cookies(&request.url, cookie)
                    .map_err(crate::cdp_page::into_setup)?;
            }
            page.goto(&request.url)?;
            let html = page.content()?;
            let cookie_header = page
                .cookie_header(&request.url)
                .inspect_err(|error| {
                    tracing::debug!(%error, key = %request.key, "Browser cookie readback unavailable")
                })
                .ok()
                .filter(|s| !s.is_empty());
            Ok(RenderedPage { html, cookie_header })
        })();

        drop(page);
        drop(slot);
        result.inspect_err(|error| {
            tracing::error!(%error, request_key = %request.key, "Browser session failed");
        })
    }

    pub fn fetch_html(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        self.render(request).map(|page| page.html)
    }
}

pub struct RecordingFetcher {
    bodies: std::collections::HashMap<String, Result<String, String>>,
    requests: Arc<std::sync::Mutex<Vec<FetchRequest>>>,
}

impl RecordingFetcher {
    pub fn new(
        bodies: std::collections::HashMap<String, Result<String, String>>,
        requests: Arc<std::sync::Mutex<Vec<FetchRequest>>>,
    ) -> Self {
        Self { bodies, requests }
    }
}

impl Fetcher for RecordingFetcher {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        self.requests
            .lock()
            .expect("request log")
            .push(FetchRequest {
                key: request.key.clone(),
                url: request.url.clone(),
                method: request.method,
                body: request.body.clone(),
                cookie: request.cookie.clone(),
                api_key: request.api_key.clone(),
                proxy: request.proxy.clone(),
                cdp_url: request.cdp_url.clone(),
            });
        match self.bodies.get(&request.key) {
            Some(Ok(body)) => Ok(body.clone()),
            Some(Err(msg)) => Err(IndexerError::Fetch(msg.clone())),
            None => Err(IndexerError::Fetch(format!("missing {}", request.key))),
        }
    }
}
