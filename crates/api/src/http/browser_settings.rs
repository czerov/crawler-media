use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use crate::http::{err, ok};
use crate::management::ApiState;
use crate::runtime_browser::{BrowserSettings, BrowserSettingsError};

pub(crate) async fn get_browser_settings(State(state): State<ApiState>) -> Response {
    let store = state.store.lock();
    match BrowserSettings::load(&store) {
        Ok(settings) => {
            let mut data = settings.response_with_store(&store);
            data["incompatible_endpoints"] =
                serde_json::to_value(state.browser.incompatible_endpoints()).unwrap_or_default();
            ok(data).into_response()
        }
        Err(error) => settings_error(error),
    }
}

pub(crate) async fn put_browser_settings(
    State(state): State<ApiState>,
    Json(body): Json<Value>,
) -> Response {
    let response = {
        let store = state.store.lock();
        match BrowserSettings::save(&store, &body) {
            Ok(settings) => {
                crate::user_agent::sync_user_agent(&store);
                Ok(settings.response_with_store(&store))
            }
            Err(error) => Err(error),
        }
    };

    match response {
        Ok(data) => {
            state.browser.clear_incompatible();
            reconcile_off_runtime(&state).await;
            ok(data).into_response()
        }
        Err(error) => settings_error(error),
    }
}

pub(crate) async fn get_obscura_status(State(state): State<ApiState>) -> Response {
    ok(serde_json::to_value(state.obscura.status()).unwrap_or_default()).into_response()
}

pub(crate) async fn post_obscura_start(State(state): State<ApiState>) -> Response {
    let policy = install_policy(&state);
    // Install/start may take minutes (download + readiness); never block a
    // tokio worker. Return 202 with the current status immediately.
    let worker = state.clone();
    tokio::task::spawn_blocking(move || match worker.obscura.start(policy) {
        Ok(()) => {
            worker.obscura.set_not_started_reason(None);
            worker.browser.set_managed_endpoint(worker.obscura.cdp_url());
        }
        Err(error) => {
            tracing::warn!(%error, "Manual Obscura start failed");
            worker.browser.set_managed_endpoint(None);
        }
    });
    (
        StatusCode::ACCEPTED,
        ok(serde_json::to_value(state.obscura.status()).unwrap_or_default()),
    )
        .into_response()
}

pub(crate) async fn post_obscura_stop(State(state): State<ApiState>) -> Response {
    let worker = state.clone();
    let _ = tokio::task::spawn_blocking(move || {
        // Clear the endpoint first so no session targets a released port.
        worker.browser.set_managed_endpoint(None);
        worker.obscura.stop();
        worker.obscura.set_not_started_reason(Some("stopped_manually"));
    })
    .await;
    ok(serde_json::to_value(state.obscura.status()).unwrap_or_default()).into_response()
}

fn install_policy(state: &ApiState) -> crate::obscura_manager::InstallPolicy {
    let store = state.store.lock();
    BrowserSettings::load(&store)
        .ok()
        .and_then(|s| s.obscura_install().parse().ok())
        .unwrap_or(crate::obscura_manager::InstallPolicy::PreinstalledOnly)
}

/// Inputs for the managed-daemon decision (plan: `ReconcileWant`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReconcileWant {
    pub switch_on: bool,
    pub instance_endpoint_configured: bool,
    pub managed_browser_sites: usize,
}

/// Pure decision: `None` = run the managed daemon, `Some(reason)` = keep it stopped.
pub fn not_started_reason(want: ReconcileWant) -> Option<&'static str> {
    if !want.switch_on {
        Some("switch_off")
    } else if want.instance_endpoint_configured {
        Some("instance_endpoint_configured")
    } else if want.managed_browser_sites == 0 {
        Some("no_managed_browser_sites")
    } else {
        None
    }
}

fn load_want(state: &ApiState) -> (ReconcileWant, crate::obscura_manager::InstallPolicy) {
    let store = state.store.lock();
    let settings = BrowserSettings::load(&store);
    if let Err(error) = &settings {
        tracing::error!(%error, "Browser settings unreadable during reconcile; treating switch as off");
    }
    let (switch_on, instance, policy) = match &settings {
        Ok(s) => (
            s.cdp_enabled(),
            !s.cdp_url().trim().is_empty(),
            s.obscura_install()
                .parse()
                .unwrap_or(crate::obscura_manager::InstallPolicy::PreinstalledOnly),
        ),
        Err(_) => (false, false, crate::obscura_manager::InstallPolicy::PreinstalledOnly),
    };
    let managed_browser_sites = store
        .list_sites()
        .map(|sites| {
            sites
                .iter()
                .filter(|s| {
                    s.enabled
                        && s.maintenance_via == domain::MaintenanceVia::Browser
                        && s.cdp_url.as_deref().is_none_or(|u| u.trim().is_empty())
                })
                .count()
        })
        .unwrap_or_else(|error| {
            tracing::error!(%error, "Listing sites failed during Browser reconcile");
            0
        });
    (
        ReconcileWant { switch_on, instance_endpoint_configured: instance, managed_browser_sites },
        policy,
    )
}

/// Blocking reconcile. Call only from a blocking context (job worker,
/// `spawn_blocking`); `ObscuraManager` serializes concurrent start/stop.
pub fn reconcile_obscura(state: &ApiState) {
    let (want, install_policy) = load_want(state);
    match not_started_reason(want) {
        None => {
            if state.obscura.owns_live_daemon() {
                state.browser.set_managed_endpoint(state.obscura.cdp_url());
                return;
            }
            tracing::info!(?want, "Reconciling Obscura: starting managed daemon");
            match state.obscura.start(install_policy) {
                Ok(()) => {
                    state.obscura.set_not_started_reason(None);
                    state.browser.set_managed_endpoint(state.obscura.cdp_url());
                }
                Err(err) => {
                    tracing::warn!(%err, "Managed Obscura failed to start in reconcile");
                    state.obscura.set_last_error(Some(err));
                    state.browser.set_managed_endpoint(None);
                }
            }
        }
        Some(reason) => {
            state.browser.set_managed_endpoint(None);
            if state.obscura.owns_live_daemon() {
                tracing::info!(reason, "Reconciling Obscura: stopping managed daemon");
                state.obscura.stop();
            }
            state.obscura.set_not_started_reason(Some(reason));
        }
    }
}

/// Async entry point: a stop decision is applied before returning (fast);
/// a start is detached onto the blocking pool so the request never waits on
/// a download.
pub async fn reconcile_off_runtime(state: &ApiState) {
    let (want, _) = {
        let s = state.clone();
        tokio::task::spawn_blocking(move || load_want(&s))
            .await
            .unwrap_or((ReconcileWant { switch_on: false, instance_endpoint_configured: false, managed_browser_sites: 0 }, crate::obscura_manager::InstallPolicy::PreinstalledOnly))
    };
    // Stop decision: clear the endpoint and record the reason now (cheap,
    // lock-free for the daemon), then let the actual stop run detached so the
    // request never waits on a concurrent download or the TERM grace period.
    if let Some(reason) = not_started_reason(want) {
        state.browser.set_managed_endpoint(None);
        state.obscura.set_not_started_reason(Some(reason));
    }
    let worker = state.clone();
    tokio::task::spawn_blocking(move || reconcile_obscura(&worker));
}

/// Fire-and-forget reconcile usable from sync code (startup, Site writes).
pub fn spawn_reconcile(state: &ApiState) {
    let worker = state.clone();
    // A stop/no-op decision is cheap (settings read, no download): apply it
    // inline so no background task contends for the store lock. Only a start
    // (possible download + readiness wait) goes to the blocking pool.
    // Inline only when nothing must be stopped (stop() can wait on the
    // lifecycle lock or the TERM grace period): just record the reason.
    let (want, _) = load_want(state);
    if let Some(reason) = not_started_reason(want) {
        state.browser.set_managed_endpoint(None);
        if !state.obscura.owns_live_daemon() {
            state.obscura.set_not_started_reason(Some(reason));
            return;
        }
    }
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn_blocking(move || reconcile_obscura(&worker));
    } else {
        reconcile_obscura(&worker);
    }
}

#[cfg(test)]
mod reconcile_tests {
    use super::*;

    fn want(on: bool, inst: bool, n: usize) -> ReconcileWant {
        ReconcileWant { switch_on: on, instance_endpoint_configured: inst, managed_browser_sites: n }
    }

    #[test]
    fn decision_matches_plan() {
        assert_eq!(not_started_reason(want(false, false, 3)), Some("switch_off"));
        assert_eq!(not_started_reason(want(true, true, 3)), Some("instance_endpoint_configured"));
        assert_eq!(not_started_reason(want(true, false, 0)), Some("no_managed_browser_sites"));
        assert_eq!(not_started_reason(want(true, false, 1)), None);
    }
}

fn settings_error(error: BrowserSettingsError) -> Response {
    tracing::error!(%error, "Browser settings operation rejected");
    let (status, code) = match &error {
        BrowserSettingsError::InvalidEndpoint(_) => {
            (StatusCode::BAD_REQUEST, "browser.invalid_endpoint")
        }
        BrowserSettingsError::Invalid(_) => (StatusCode::BAD_REQUEST, "browser.invalid_config"),
        BrowserSettingsError::Store(_) => (StatusCode::INTERNAL_SERVER_ERROR, "store.error"),
    };
    err(status, code, &error.to_string())
}
