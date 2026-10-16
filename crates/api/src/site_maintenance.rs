use domain::Site;
use hooks::{Bus, CheckInPlugin, LoginPlugin, PluginError, SiteCredentials};

use crate::management::ApiState;

pub fn perform_login(
    state: &ApiState,
    site: &Site,
    bus: &Bus,
    creds: &dyn SiteCredentials,
) -> Result<(), PluginError> {
    let policy = crate::check_in::site_policy(state, site)?;
    let http = crate::check_in::attendance_http();

    match site.maintenance_via {
        domain::MaintenanceVia::Http => {
            LoginPlugin::http(bus, creds, &http)
                .with_policy(&policy)
                .login(site.id)
        }
        domain::MaintenanceVia::Browser => {
            let cdp_resolves = state
                .browser
                .effective_config()
                .ok()
                .and_then(|c| c.resolve(site.cdp_url.as_deref()).ok().flatten())
                .is_some();

            if cdp_resolves {
                tracing::info!(site_id = %site.id, site_name = %site.name, "Attempting browser login");
                match LoginPlugin::browser(bus, creds, &state.browser)
                    .with_policy(&policy)
                    .login(site.id)
                {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        tracing::warn!(
                            site_id = %site.id,
                            site_name = %site.name,
                            %error,
                            "Browser login failed, falling back to HTTP"
                        );
                        LoginPlugin::http(bus, creds, &http)
                            .with_policy(&policy)
                            .login(site.id)
                    }
                }
            } else {
                tracing::warn!(
                    site_id = %site.id,
                    site_name = %site.name,
                    "Browser endpoint unavailable or CDP disabled for browser login, falling back to HTTP"
                );
                LoginPlugin::http(bus, creds, &http)
                    .with_policy(&policy)
                    .login(site.id)
            }
        }
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

    match site.maintenance_via {
        domain::MaintenanceVia::Http => {
            CheckInPlugin::http(bus, creds, &http)
                .with_policy(&policy)
                .check_in(site.id)
        }
        domain::MaintenanceVia::Browser => {
            let cdp_resolves = state
                .browser
                .effective_config()
                .ok()
                .and_then(|c| c.resolve(site.cdp_url.as_deref()).ok().flatten())
                .is_some();

            if cdp_resolves {
                tracing::info!(site_id = %site.id, site_name = %site.name, "Attempting browser check-in");
                match CheckInPlugin::browser(bus, creds, &state.browser)
                    .with_policy(&policy)
                    .check_in(site.id)
                {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        tracing::warn!(
                            site_id = %site.id,
                            site_name = %site.name,
                            %error,
                            "Browser check-in failed, falling back to HTTP"
                        );
                        CheckInPlugin::http(bus, creds, &http)
                            .with_policy(&policy)
                            .check_in(site.id)
                    }
                }
            } else {
                tracing::warn!(
                    site_id = %site.id,
                    site_name = %site.name,
                    "Browser endpoint unavailable or CDP disabled for browser check-in, falling back to HTTP"
                );
                CheckInPlugin::http(bus, creds, &http)
                    .with_policy(&policy)
                    .check_in(site.id)
            }
        }
    }
}
