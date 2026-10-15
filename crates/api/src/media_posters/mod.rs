use std::sync::Arc;
use domain::MediaId;

pub mod cache;
pub mod http;
pub mod inflight;
pub mod service;

#[derive(Clone, Debug)]
pub struct PosterBytes {
    pub content_type: String,
    pub bytes: Arc<[u8]>,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum PosterError {
    #[error("media or poster not found")]
    NotFound,
    #[error("poster request timed out")]
    Timeout,
    #[error("invalid upstream image")]
    InvalidImage,
    #[error("poster upstream failed: {0}")]
    Upstream(String),
    #[error("poster IO failed: {0}")]
    Io(String),
}

#[async_trait::async_trait]
pub trait MediaPosterSource: Send + Sync {
    async fn get(&self, media_id: MediaId) -> Result<PosterBytes, PosterError>;
}
