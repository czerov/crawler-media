//! HTTP handlers for Library verification and missing row pruning.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use domain::MediaId;
use serde::{Deserialize, Serialize};

use crate::library_presence::{PresenceError, prune, verify};
use crate::management::ApiState;
#[derive(Serialize)]
pub struct PresenceResponseData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted: Option<usize>,
    pub restored: usize,
    pub skipped: usize,
}

#[derive(Serialize)]
pub struct PresenceEnvelope {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<PresenceResponseData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<PresenceErrorPayload>,
}

#[derive(Serialize)]
pub struct PresenceErrorPayload {
    pub code: String,
    pub message: String,
}

#[derive(Deserialize)]
pub struct DeleteMissingQuery {
    pub media_item_id: Option<String>,
}

fn map_presence_error(err: PresenceError) -> (StatusCode, Json<PresenceEnvelope>) {
    let (status, code, msg) = match err {
        PresenceError::Missing => (
            StatusCode::NOT_FOUND,
            "library.missing",
            "Library not found",
        ),
        PresenceError::Deleting => (
            StatusCode::CONFLICT,
            "library.deleting",
            "Library is deleting or frozen",
        ),
        PresenceError::Ambiguous => (
            StatusCode::CONFLICT,
            "library.ownership_ambiguous",
            "Ambiguous library ownership",
        ),
        PresenceError::RootUnavailable => (
            StatusCode::CONFLICT,
            "library.root_unavailable",
            "Library root directory is unavailable or not a directory",
        ),
        PresenceError::ScopeChanged => (
            StatusCode::CONFLICT,
            "library.scope_changed",
            "Library configuration or scope changed during operation",
        ),
        PresenceError::Busy => (
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "System is currently busy",
        ),
        PresenceError::Store(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "Internal database error",
        ),
    };

    (
        status,
        Json(PresenceEnvelope {
            ok: false,
            data: None,
            error: Some(PresenceErrorPayload {
                code: code.to_string(),
                message: msg.to_string(),
            }),
        }),
    )
}

pub(crate) async fn verify_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> (StatusCode, Json<PresenceEnvelope>) {
    let state_clone = state.clone();
    let res = state
        .http_work
        .run_to_completion(move || verify(&state_clone, &id))
        .await;

    let res = match res {
        Ok(r) => r,
        Err(_) => return map_presence_error(PresenceError::Busy),
    };

    match res {
        Ok(outcome) => (
            StatusCode::OK,
            Json(PresenceEnvelope {
                ok: true,
                data: Some(PresenceResponseData {
                    missing: Some(outcome.missing),
                    deleted: None,
                    restored: outcome.restored,
                    skipped: outcome.skipped,
                }),
                error: None,
            }),
        ),
        Err(err) => map_presence_error(err),
    }
}

pub(crate) async fn prune_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(query): Query<DeleteMissingQuery>,
) -> (StatusCode, Json<PresenceEnvelope>) {
    let media_id = if let Some(mid_str) = query.media_item_id {
        match mid_str.parse::<MediaId>() {
            Ok(mid) => Some(mid),
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(PresenceEnvelope {
                        ok: false,
                        data: None,
                        error: Some(PresenceErrorPayload {
                            code: "invalid_media_id".to_string(),
                            message: "Invalid media_item_id UUID".to_string(),
                        }),
                    }),
                );
            }
        }
    } else {
        None
    };

    let state_clone = state.clone();
    let res = state
        .http_work
        .run_to_completion(move || prune(&state_clone, &id, media_id))
        .await;

    let res = match res {
        Ok(r) => r,
        Err(_) => return map_presence_error(PresenceError::Busy),
    };

    match res {
        Ok(outcome) => (
            StatusCode::OK,
            Json(PresenceEnvelope {
                ok: true,
                data: Some(PresenceResponseData {
                    missing: None,
                    deleted: Some(outcome.deleted),
                    restored: outcome.restored,
                    skipped: outcome.skipped,
                }),
                error: None,
            }),
        ),
        Err(err) => map_presence_error(err),
    }
}
