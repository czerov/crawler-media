use std::sync::Arc;
use indexer::{BrowserConfig, BrowserRoute, FetchMethod, FetchRequest, IndexerError, PageSession};

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
        cdp_url: cdp.map(str::to_owned),
    }
}

#[test]
fn switch_off_resolves_to_none_even_with_a_site_endpoint() {
    let mut config = BrowserConfig::disabled();
    config.enabled = false;
    assert_eq!(config.resolve(Some("http://site.invalid:9222")).unwrap(), None);
}

#[test]
fn switch_on_prefers_site_over_instance_over_managed() {
    let config = BrowserConfig {
        enabled: true,
        endpoint: Some("http://instance.invalid:9222".into()),
        managed_endpoint: Some("http://managed.invalid:9223".into()),
    };
    let (url, route) = config.resolve(Some("http://site.invalid:9222")).unwrap().unwrap();
    assert_eq!(url, "http://site.invalid:9222");
    assert_eq!(route, BrowserRoute::Site);
}

#[test]
fn switch_on_uses_instance_endpoint_when_site_is_absent() {
    let config = BrowserConfig {
        enabled: true,
        endpoint: Some("http://instance.invalid:9222".into()),
        managed_endpoint: Some("http://managed.invalid:9223".into()),
    };
    let (url, route) = config.resolve(None).unwrap().unwrap();
    assert_eq!(url, "http://instance.invalid:9222");
    assert_eq!(route, BrowserRoute::Instance);
}

#[test]
fn switch_on_uses_managed_endpoint_when_nothing_is_configured() {
    let config = BrowserConfig {
        enabled: true,
        endpoint: None,
        managed_endpoint: Some("http://managed.invalid:9223".into()),
    };
    let (url, route) = config.resolve(None).unwrap().unwrap();
    assert_eq!(url, "http://managed.invalid:9223");
    assert_eq!(route, BrowserRoute::Managed);
}

#[test]
fn switch_on_without_any_endpoint_is_a_setup_error() {
    let config = BrowserConfig {
        enabled: true,
        endpoint: None,
        managed_endpoint: None,
    };
    assert!(config.resolve(None).is_err());
}

#[test]
fn empty_site_endpoint_is_rejected_not_silently_rerouted() {
    let config = BrowserConfig {
        enabled: true,
        endpoint: Some("http://instance.invalid:9222".into()),
        managed_endpoint: Some("http://managed.invalid:9223".into()),
    };
    assert!(config.resolve(Some("")).is_err());
    assert!(config.resolve(Some("   ")).is_err());
}

#[test]
fn endpoint_validation_rejects_credentials_query_and_ws() {
    assert!(BrowserConfig::validate_endpoint("ws://site.invalid:9222").is_err());
    assert!(BrowserConfig::validate_endpoint("wss://site.invalid:9222").is_err());
    assert!(BrowserConfig::validate_endpoint("http://user:pass@site.invalid:9222").is_err());
    assert!(BrowserConfig::validate_endpoint("http://site.invalid:9222/json?query=1").is_err());
    assert!(BrowserConfig::validate_endpoint("http://site.invalid:9222/").is_ok());
}

#[test]
fn render_profile_flag_is_ignored_and_search_uses_http() {
    let overlay = tempfile::tempdir().unwrap();
    std::fs::write(
        overlay.path().join("demo.yaml"),
        r#"
id: demo
framework: nexusphp
render: true
search:
  path: /torrents.php
  query_param: search
list:
  item: "tr.torrent"
fields:
  title:
    selector: "a.name"
    attr: title
  enclosure:
    selector: "a.download"
    attr: href
"#,
    )
    .unwrap();
    let site_id = domain::SiteId::new();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let fetcher = indexer::RecordingFetcher::new(
        std::collections::HashMap::from([(
            format!("search:{site_id}"),
            Ok(include_str!("fixtures/nexusphp.html").into()),
        )]),
        requests.clone(),
    );
    let indexer = indexer::Indexer::new(
        indexer::ProfileSet::load(Some(overlay.path())).unwrap(),
        Arc::new(fetcher),
    );
    let site = domain::Site {
        id: site_id,
        name: "demo".into(),
        url: "https://pt.example/".into(),
        profile_id: "demo".into(),
        cookie: None,
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: Some("http://site.invalid:9222".into()),
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
    };
    let outcome = indexer.search(&[site], "matrix");
    assert_eq!(outcome.torrents.len(), 1);
    assert_eq!(requests.lock().unwrap().len(), 1);
}
