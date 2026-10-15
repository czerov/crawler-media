use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use domain::MediaId;
use std::str::FromStr;

use crate::management::ApiState;
use crate::media_posters::PosterError;

pub(crate) async fn get_media_poster(
    State(state): State<ApiState>,
    Path(id_str): Path<String>,
) -> Response {
    let media_id = match MediaId::from_str(&id_str) {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid media id").into_response(),
    };

    match state.media_posters.get(media_id).await {
        Ok(poster) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, poster.content_type),
                (header::CACHE_CONTROL, "public, max-age=86400".into()),
            ],
            poster.bytes.to_vec(),
        )
            .into_response(),
        Err(PosterError::NotFound) => (
            StatusCode::NOT_FOUND,
            [(header::CACHE_CONTROL, "no-store")],
            "poster not found",
        )
            .into_response(),
        Err(PosterError::Timeout) => (StatusCode::GATEWAY_TIMEOUT, "poster fetch timed out").into_response(),
        Err(PosterError::InvalidImage | PosterError::Upstream(_)) => {
            (StatusCode::BAD_GATEWAY, "failed to fetch upstream poster").into_response()
        }
        Err(PosterError::Io(_)) => (StatusCode::INTERNAL_SERVER_ERROR, "poster io error").into_response(),
        Err(PosterError::Busy) => (StatusCode::SERVICE_UNAVAILABLE, "poster service busy").into_response(),
    }
}
