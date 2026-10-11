//! Jellyfin/Emby system, authentication and device endpoints.

use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::provider::AuthenticateError;

use super::AppState;

#[derive(Deserialize)]
pub struct AuthBody {
    #[serde(alias = "Username")]
    pub username: String,
    #[serde(alias = "Pw")]
    pub pw: String,
}
pub(super) async fn public_system_info(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "Id": state.provider.server_id(),
        "ServerName": "crawler-media",
        "ProductName": "crawler-media",
        "Version": "0.1.0",
        "StartupWizardCompleted": true,
    }))
}

pub(super) async fn ping() -> Response {
    StatusCode::OK.into_response()
}

pub(super) async fn device_description(State(state): State<AppState>) -> Response {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><root xmlns=\"urn:schemas-upnp-org:device-1-0\"><specVersion><major>1</major><minor>0</minor></specVersion><device><deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType><friendlyName>crawler-media</friendlyName><manufacturer>crawler-media</manufacturer><modelName>crawler-media</modelName><UDN>uuid:{}</UDN><presentationURL>/</presentationURL></device></root>",
        state.provider.server_id()
    );
    (
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

pub(super) async fn authenticate_by_name(
    State(state): State<AppState>,
    Json(body): Json<AuthBody>,
) -> Result<Json<Value>, StatusCode> {
    let res = match state
        .provider
        .authenticate_password(&body.username, &body.pw)
        .await
    {
        Ok(Some(res)) => res,
        Ok(None) => return Err(StatusCode::UNAUTHORIZED),
        Err(AuthenticateError::Busy) => return Err(StatusCode::SERVICE_UNAVAILABLE),
        Err(AuthenticateError::Internal(message)) => {
            tracing::error!(%message, "Jellyfin 密码登录失败");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    Ok(Json(json!({
        "AccessToken": res.0,
        "User": { "Id": res.1.id.to_string(), "Name": res.1.login },
    })))
}
pub(super) async fn quickconnect_enabled() -> Json<Value> {
    Json(json!(false))
}
