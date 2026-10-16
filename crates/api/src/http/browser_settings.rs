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
    let store = state.store.lock();
    match BrowserSettings::save(&store, &body) {
        Ok(settings) => {
            crate::user_agent::sync_user_agent(&store);
            ok(settings.response_with_store(&store)).into_response()
        }
        Err(error) => settings_error(error),
    }
}

fn settings_error(error: BrowserSettingsError) -> Response {
    tracing::error!(%error, "Browser settings operation rejected");
    let (status, code) = match &error {
        BrowserSettingsError::Invalid(_) => (StatusCode::BAD_REQUEST, "browser.invalid_config"),
        BrowserSettingsError::Store(_) => (StatusCode::INTERNAL_SERVER_ERROR, "store.error"),
    };
    err(status, code, &error.to_string())
}
