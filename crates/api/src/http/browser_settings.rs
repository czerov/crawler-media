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
        Ok(settings) => ok(settings.response_with_store(&store)).into_response(),
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
            reconcile_obscura(&state);
            let _ = jobs::Queue::open(state.store.lock().sqlite_path())
                .and_then(|q| q.enqueue(jobs::JobKind::BrowserReconcile, "{}", 0));
            ok(data).into_response()
        }
        Err(error) => settings_error(error),
    }
}

pub(crate) async fn get_obscura_status(State(state): State<ApiState>) -> Response {
    ok(serde_json::to_value(state.obscura.status()).unwrap_or_default()).into_response()
}

pub(crate) async fn post_obscura_start(State(state): State<ApiState>) -> Response {
    let policy = {
        let store = state.store.lock();
        store
            .get_setting(crate::settings_keys::OBSCURA_INSTALL)
            .ok()
            .flatten()
            .and_then(|s| s.parse::<crate::obscura_manager::InstallPolicy>().ok())
            .unwrap_or(crate::obscura_manager::InstallPolicy::PreinstalledOnly)
    };

    match state.obscura.start(policy) {
        Ok(()) => {
            state.obscura.set_not_started_reason(None);
            ok(serde_json::to_value(state.obscura.status()).unwrap_or_default()).into_response()
        }
        Err(e) => err(
            StatusCode::BAD_REQUEST,
            "browser.obscura_start_failed",
            &e,
        ),
    }
}

pub(crate) async fn post_obscura_stop(State(state): State<ApiState>) -> Response {
    state.obscura.stop();
    ok(serde_json::to_value(state.obscura.status()).unwrap_or_default()).into_response()
}

pub fn reconcile_obscura(state: &ApiState) {
    let (cdp_enabled, cdp_url, install_policy) = {
        let store = state.store.lock();
        let enabled = store
            .get_setting(crate::settings_keys::CDP_ENABLED)
            .ok()
            .flatten()
            .map(|v| v == "1")
            .unwrap_or(false);
        let url = store
            .get_setting(crate::settings_keys::CDP_URL)
            .ok()
            .flatten()
            .filter(|u| !u.trim().is_empty());
        let install = store
            .get_setting(crate::settings_keys::OBSCURA_INSTALL)
            .ok()
            .flatten()
            .and_then(|s| s.parse::<crate::obscura_manager::InstallPolicy>().ok())
            .unwrap_or(crate::obscura_manager::InstallPolicy::PreinstalledOnly);
        (enabled, url, install)
    };

    if cdp_enabled && cdp_url.is_none() {
        if !state.obscura.owns_live_daemon() {
            tracing::info!("Reconciling Obscura: starting managed daemon");
            match state.obscura.start(install_policy) {
                Ok(()) => {
                    tracing::info!("Managed Obscura started successfully in reconcile");
                    state.obscura.set_not_started_reason(None);
                }
                Err(err) => {
                    tracing::warn!(%err, "Managed Obscura failed to start in reconcile");
                    state.obscura.set_last_error(Some(err));
                }
            }
        }
    } else {
        if state.obscura.owns_live_daemon() {
            tracing::info!("Reconciling Obscura: stopping managed daemon");
            state.obscura.stop();
        }
        if !cdp_enabled {
            state.obscura.set_not_started_reason(Some("cdp_disabled"));
        } else {
            state.obscura.set_not_started_reason(Some("external_endpoint_configured"));
        }
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
