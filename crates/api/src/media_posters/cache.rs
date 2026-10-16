use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use domain::MediaId;
use super::{PosterBytes, PosterError};

pub const POSITIVE_TTL: Duration = Duration::from_secs(24 * 3600);
pub const NEGATIVE_TTL: Duration = Duration::from_secs(30);
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_META_BYTES: usize = 1024;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Clone)]
pub struct DiskPosterCache {
    root: PathBuf,
}

impl DiskPosterCache {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    pub fn default_dir() -> PathBuf {
        crate::config::data_dir_from_env()
            .join("cache")
            .join("media-posters")
    }

    fn file_path(&self, media_id: MediaId) -> PathBuf {
        self.root.join(format!("{media_id}.img"))
    }

    pub fn get(&self, media_id: MediaId) -> Option<PosterBytes> {
        let path = self.file_path(media_id);
        let bytes = std::fs::read(&path).ok()?;
        if bytes.len() > MAX_IMAGE_BYTES + MAX_META_BYTES + 2 {
            let _ = std::fs::remove_file(&path);
            return None;
        }

        let (meta, img) = split_cache_payload(&bytes)?;
        if img.len() > MAX_IMAGE_BYTES || meta.len() > MAX_META_BYTES {
            let _ = std::fs::remove_file(&path);
            return None;
        }
        let mut lines = meta.lines();
        let fetched_at = lines.next()?.parse::<u64>().ok()?;
        let content_type = lines.next()?.to_string();

        let age = Duration::from_secs(now_secs().saturating_sub(fetched_at));
        if age > POSITIVE_TTL {
            let _ = std::fs::remove_file(&path);
            return None;
        }

        Some(PosterBytes {
            content_type,
            bytes: Arc::from(img),
        })
    }

    pub fn put(&self, media_id: MediaId, content_type: &str, img: &[u8]) -> Result<(), PosterError> {
        if img.len() > MAX_IMAGE_BYTES {
            return Err(PosterError::InvalidImage);
        }
        let meta_prefix = format!("{}\n{}\n", now_secs(), content_type);
        if meta_prefix.len() > MAX_META_BYTES {
            return Err(PosterError::InvalidImage);
        }
        if let Err(e) = std::fs::create_dir_all(&self.root) {
            return Err(PosterError::Io(format!("create cache dir: {e}")));
        }

        let target = self.file_path(media_id);
        let tmp = self.root.join(format!("{media_id}.tmp.{}.{:?}", std::process::id(), std::thread::current().id()));

        let meta = format!("{meta_prefix}\n");
        let mut file = std::fs::File::create(&tmp).map_err(|e| PosterError::Io(e.to_string()))?;
        file.write_all(meta.as_bytes()).map_err(|e| PosterError::Io(e.to_string()))?;
        file.write_all(img).map_err(|e| PosterError::Io(e.to_string()))?;
        file.sync_all().map_err(|e| PosterError::Io(e.to_string()))?;
        drop(file);

        std::fs::rename(&tmp, &target).map_err(|e| PosterError::Io(e.to_string()))?;
        Ok(())
    }
}

fn split_cache_payload(payload: &[u8]) -> Option<(String, &[u8])> {
    let mut double_nl = None;
    for i in 0..payload.len().saturating_sub(1) {
        if payload[i] == b'\n' && payload[i + 1] == b'\n' {
            double_nl = Some(i);
            break;
        }
    }
    let idx = double_nl?;
    let meta = String::from_utf8(payload[..idx].to_vec()).ok()?;
    Some((meta, &payload[idx + 2..]))
}
