use domain::Site;
use hooks::{Bus, CheckInPlugin, LoginPlugin, PluginError, SiteCredentials};

use crate::management::ApiState;

/// Whether a browser endpoint resolves for this site. Configuration errors are
/// surfaced (not swallowed); `Ok(false)` means CDP is disabled / no endpoint.
fn browser_endpoint_resolves(state: &ApiState, site: &Site) -> Result<bool, PluginError> {
    let config = state
        .browser
        .effective_config()
        .map_err(|e| PluginError::Fetch(format!("Browser 配置错误: {e}")))?;
    let resolved = config
        .resolve(site.cdp_url.as_deref())
        .map_err(|e| PluginError::Fetch(format!("Browser 端点配置错误: {e}")))?;
    Ok(resolved.is_some())
}

/// Run a browser-routed maintenance step. Only failures that happened before
/// navigation (`BrowserPreNavigation`) fall back to HTTP; anything after the
/// page was navigated is returned as-is so the action is never submitted twice.
pub(crate) fn run_with_browser_fallback(
    site: &Site,
    action: &str,
    endpoint_resolves: Result<bool, PluginError>,
    browser: impl FnOnce() -> Result<(), PluginError>,
    http: impl FnOnce(bool) -> Result<(), PluginError>,
) -> Result<(), PluginError> {
    if !endpoint_resolves? {
        tracing::warn!(site_id = %site.id, site_name = %site.name, action,
            "Browser CDP disabled or no endpoint, falling back to HTTP");
        return http(false);
    }
    tracing::info!(site_id = %site.id, site_name = %site.name, action, "Attempting browser maintenance");
    match browser() {
        Ok(()) => Ok(()),
        Err(error) if error.is_browser_pre_navigation() => {
            tracing::warn!(site_id = %site.id, site_name = %site.name, action, %error,
                "Browser failed before navigation, falling back to HTTP");
            // The browser attempt already emitted the hook event.
            http(true)
        }
        Err(error) => {
            tracing::error!(site_id = %site.id, site_name = %site.name, action, %error,
                "Browser failed after navigation; not falling back to HTTP");
            Err(error)
        }
    }
}

pub fn perform_login(
    state: &ApiState,
    site: &Site,
    bus: &Bus,
    creds: &dyn SiteCredentials,
) -> Result<(), PluginError> {
    let policy = crate::check_in::site_policy(state, site)?;
    let http = crate::check_in::attendance_http();
    let via_http = |fallback: bool| {
        let plugin = LoginPlugin::http(bus, creds, &http).with_policy(&policy);
        if fallback { plugin.without_hook_event() } else { plugin }.login(site.id)
    };
    match site.maintenance_via {
        domain::MaintenanceVia::Http => via_http(false),
        domain::MaintenanceVia::Browser => run_with_browser_fallback(
            site,
            "login",
            browser_endpoint_resolves(state, site),
            || {
                LoginPlugin::browser(bus, creds, &state.browser)
                    .with_policy(&policy)
                    .login(site.id)
            },
            via_http,
        ),
    }
}

pub fn perform_check_in(
    state: &ApiState,
    site: &Site,
    bus: &Bus,
    creds: &dyn SiteCredentials,
) -> Result<(), PluginError> {
    let policy = crate::check_in::site_policy(state, site)?;
    let http = crate::check_in::attendance_http();
    let via_http = |fallback: bool| {
        let plugin = CheckInPlugin::http(bus, creds, &http).with_policy(&policy);
        if fallback { plugin.without_hook_event() } else { plugin }.check_in(site.id)
    };
    match site.maintenance_via {
        domain::MaintenanceVia::Http => via_http(false),
        domain::MaintenanceVia::Browser => run_with_browser_fallback(
            site,
            "check_in",
            browser_endpoint_resolves(state, site),
            || {
                CheckInPlugin::browser(bus, creds, &state.browser)
                    .with_policy(&policy)
                    .check_in(site.id)
            },
            via_http,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn site() -> Site {
        Site {
            id: domain::SiteId::new(),
            name: "t".into(),
            url: "https://example.org/".into(),
            profile_id: "nexusphp".into(),
            cookie: Some("a=b".into()),
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: domain::MaintenanceVia::Browser,
        }
    }

    fn run(
        resolves: Result<bool, PluginError>,
        browser: Result<(), PluginError>,
    ) -> (Result<(), PluginError>, u32) {
        let http_calls = Cell::new(0u32);
        let browser_was_pre_nav = matches!(browser, Err(PluginError::BrowserPreNavigation(_)));
        let s = site();
        let r = run_with_browser_fallback(&s, "check_in", resolves, || browser, |fallback| {
            assert_eq!(fallback, matches!(browser_was_pre_nav, true));
            http_calls.set(http_calls.get() + 1);
            Ok(())
        });
        (r, http_calls.get())
    }

    #[test]
    fn post_navigation_error_does_not_fall_back_no_double_check_in() {
        let (r, calls) = run(Ok(true), Err(PluginError::Fetch("after goto".into())));
        assert!(r.is_err());
        assert_eq!(calls, 0, "post-navigation failure must not trigger HTTP check-in");
        let (r, calls) = run(Ok(true), Err(PluginError::Veto("policy".into())));
        assert!(r.is_err());
        assert_eq!(calls, 0);
    }

    #[test]
    fn pre_navigation_error_falls_back_once() {
        let (r, calls) = run(Ok(true), Err(PluginError::BrowserPreNavigation("busy".into())));
        assert!(r.is_ok());
        assert_eq!(calls, 1);
    }

    #[test]
    fn browser_success_does_not_call_http() {
        let (r, calls) = run(Ok(true), Ok(()));
        assert!(r.is_ok());
        assert_eq!(calls, 0);
    }

    #[test]
    fn disabled_endpoint_uses_http_and_config_error_surfaces() {
        let (r, calls) = run(Ok(false), Ok(()));
        assert!(r.is_ok());
        assert_eq!(calls, 1);
        let (r, calls) = run(Err(PluginError::Fetch("bad config".into())), Ok(()));
        assert!(r.is_err());
        assert_eq!(calls, 0, "config errors must not be swallowed into HTTP fallback");
    }
}
