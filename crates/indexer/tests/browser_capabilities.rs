use indexer::{Browser, BrowserConfig, FetchMethod, FetchRequest, IndexerError, PageSession};
use std::sync::Arc;

struct EndpointPage(String);
impl PageSession for EndpointPage {
    fn goto(&self, _: &str) -> Result<(), IndexerError> {
        Ok(())
    }
    fn set_cookie_header(&self, _: &str) -> Result<(), IndexerError> {
        Ok(())
    }
    fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> {
        Ok(())
    }
    fn content(&self) -> Result<String, IndexerError> {
        Ok(self.0.clone())
    }
}

fn request(cdp: Option<&str>) -> FetchRequest {
    FetchRequest {
        key: "browser-route".into(),
        url: "https://fixture.invalid/document".into(),
        method: FetchMethod::Get,
        body: None,
        cookie: None,
        api_key: None,
        proxy: None,
        render: true,
        cdp_url: cdp.map(str::to_owned),
    }
}

fn browser(config: BrowserConfig) -> Browser {
    Browser::with_opener(config, |endpoint| {
        Ok(Arc::new(EndpointPage(
            endpoint.unwrap_or("missing").to_owned(),
        )))
    })
}

#[test]
fn managed_enable_rejects_unavailable_capability_without_creating_placeholder() {
    let data = tempfile::tempdir().unwrap();
    let result = BrowserConfig::enable_in(data.path());
    assert!(
        result.is_err(),
        "managed Chromium is not implemented: {result:?}"
    );
    assert!(!data.path().join("chromium").exists());
}

#[test]
fn injected_opener_receives_selected_obscura_endpoint() {
    let browser = browser(
        BrowserConfig::disabled().with_obscura(true, Some("http://obscura.invalid:9223".into())),
    );
    assert_eq!(
        browser.fetch_html(&request(None)).unwrap(),
        "http://obscura.invalid:9223"
    );
}

#[test]
fn explicit_site_cdp_overrides_obscura_for_that_site() {
    let browser = browser(
        BrowserConfig::disabled().with_obscura(true, Some("http://obscura.invalid:9223".into())),
    );
    assert_eq!(
        browser
            .fetch_html(&request(Some("http://site.invalid:9222")))
            .unwrap(),
        "http://site.invalid:9222"
    );
}

#[test]
fn disabled_obscura_does_not_route_to_its_stored_endpoint() {
    let browser = browser(
        BrowserConfig::disabled().with_obscura(false, Some("http://obscura.invalid:9223".into())),
    );
    assert!(browser.fetch_html(&request(None)).is_err());
}

#[test]
fn injected_session_cannot_bypass_missing_endpoint_validation() {
    assert!(
        browser(BrowserConfig::disabled())
            .fetch_html(&request(None))
            .is_err()
    );
}

#[test]
fn incomplete_obscura_does_not_silently_fall_back() {
    let browser = browser(BrowserConfig::disabled().with_obscura(true, None));
    assert!(browser.fetch_html(&request(None)).is_err());
}

#[test]
fn managed_flag_cannot_be_bypassed_by_site_endpoint_or_injected_session() {
    let mut config = BrowserConfig::disabled();
    config.enabled = true;
    let result = browser(config).fetch_html(&request(Some("http://site.invalid:9222")));
    assert!(
        result.is_err(),
        "unsupported managed capability must be rejected: {result:?}"
    );
}

#[test]
fn invalid_explicit_site_endpoint_does_not_fall_back_to_obscura() {
    let browser = browser(
        BrowserConfig::disabled().with_obscura(true, Some("http://obscura.invalid:9223".into())),
    );
    assert!(browser.fetch_html(&request(Some(""))).is_err());
}

#[test]
fn unsupported_websocket_discovery_endpoint_is_rejected_before_opening() {
    let browser = browser(BrowserConfig::disabled());
    assert!(
        browser
            .fetch_html(&request(Some("ws://site.invalid:9222")))
            .is_err()
    );
}
