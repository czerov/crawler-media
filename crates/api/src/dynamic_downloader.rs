//! Runtime-resolved Downloader proxy.
//!
//! The selected Downloader is configuration, not process identity: users may
//! edit or switch the default without restarting the API. This proxy resolves
//! the current effective config before each operation and caches the connected
//! client until that config changes.

use std::path::Path;
use std::sync::Arc;

use domain::Torrent;
use downloader::{
    Downloader, DownloaderError, MemoryDownloader, QbitDownloader, TransmissionDownloader,
};
use parking_lot::Mutex;

use crate::Store;
use crate::runtime_downloader::{ChosenDownloader, DownloaderEnv, choose_downloader};

type CachedClient = Option<(ChosenDownloader, Arc<dyn Downloader>)>;

pub struct DynamicDownloader {
    store: Arc<Mutex<Store>>,
    env: DownloaderEnv,
    memory: Arc<MemoryDownloader>,
    cached: Mutex<CachedClient>,
}

impl DynamicDownloader {
    pub fn new(store: Arc<Mutex<Store>>, env: DownloaderEnv, data_dir: &Path) -> Self {
        Self {
            store,
            env,
            memory: Arc::new(MemoryDownloader::new(data_dir.join("stage"))),
            cached: Mutex::new(None),
        }
    }

    fn client(&self) -> Result<Arc<dyn Downloader>, DownloaderError> {
        let chosen = choose_downloader(&self.store.lock(), &self.env)
            .map_err(|error| DownloaderError::Message(error.to_string()))?;
        if let Some((cached_config, client)) = self.cached.lock().as_ref() {
            if cached_config == &chosen {
                return Ok(client.clone());
            }
        }
        let client = connect_chosen(&chosen, self.memory.clone())?;
        *self.cached.lock() = Some((chosen, client.clone()));
        Ok(client)
    }
}

/// Connect one concrete runtime config. Explicitly selected downloaders use
/// this path too; connection failures are returned rather than silently
/// pretending the in-memory fallback accepted the task.
pub fn connect_chosen(
    chosen: &ChosenDownloader,
    memory: Arc<MemoryDownloader>,
) -> Result<Arc<dyn Downloader>, DownloaderError> {
    match chosen {
        ChosenDownloader::Qbittorrent(config) => QbitDownloader::connect(config.clone())
            .map(|client| Arc::new(client) as Arc<dyn Downloader>),
        ChosenDownloader::Transmission(config) => TransmissionDownloader::connect(config.clone())
            .map(|client| Arc::new(client) as Arc<dyn Downloader>),
        ChosenDownloader::Memory => Ok(memory),
        ChosenDownloader::Unavailable => Err(DownloaderError::Message(
            "Downloader 配置不可用，未接受下载".into(),
        )),
    }
}

macro_rules! delegate {
    ($self:ident, $method:ident ( $($arg:expr),* )) => {{
        $self.client()?.$method($($arg),*)
    }};
}

impl Downloader for DynamicDownloader {
    fn remember_identity(&self, torrent: &Torrent, hash: &str) -> Result<(), DownloaderError> {
        delegate!(self, remember_identity(torrent, hash))
    }

    fn select_files(
        &self,
        torrent: &Torrent,
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<Option<usize>, DownloaderError> {
        delegate!(self, select_files(torrent, keep))
    }

    fn add(&self, torrent: &Torrent) -> Result<(), DownloaderError> {
        delegate!(self, add(torrent))
    }

    fn add_with_options(
        &self,
        torrent: &Torrent,
        download_url: &str,
        save_path: Option<&str>,
    ) -> Result<(), DownloaderError> {
        delegate!(self, add_with_options(torrent, download_url, save_path))
    }

    fn add_resolved(&self, torrent: &Torrent, download_url: &str) -> Result<(), DownloaderError> {
        delegate!(self, add_resolved(torrent, download_url))
    }

    fn completed_files(
        &self,
        torrent: &Torrent,
    ) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        delegate!(self, completed_files(torrent))
    }

    fn is_claimed(&self, torrent: &Torrent) -> Result<bool, DownloaderError> {
        delegate!(self, is_claimed(torrent))
    }

    fn collectable_files(
        &self,
        torrent: &Torrent,
    ) -> Result<(Vec<std::path::PathBuf>, bool), DownloaderError> {
        delegate!(self, collectable_files(torrent))
    }

    fn uploaded_by_category(&self, category: &str) -> Result<u64, DownloaderError> {
        delegate!(self, uploaded_by_category(category))
    }

    fn remove(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        delegate!(self, remove(torrent, delete_files))
    }

    fn remove_owned(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        delegate!(self, remove_owned(torrent, delete_files))
    }

    fn owned_identity(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        delegate!(self, owned_identity(torrent))
    }

    fn endpoint(&self) -> Option<String> {
        // Report the endpoint of the client actually selected right now: this
        // carries the startup-parsed configuration, including secret files.
        match self.client() {
            Ok(client) => client.endpoint(),
            Err(error) => {
                tracing::error!(%error, "无法解析当前下载器端点");
                None
            }
        }
    }

    fn snapshot_client(&self) -> Result<Option<Arc<dyn Downloader>>, DownloaderError> {
        self.client().map(Some)
    }

    fn delete_task(&self, info_hash: &str, delete_files: bool) -> Result<(), DownloaderError> {
        delegate!(self, delete_task(info_hash, delete_files))
    }

    fn pause_task(&self, info_hash: &str) -> Result<(), DownloaderError> {
        delegate!(self, pause_task(info_hash))
    }

    fn resume_task(&self, info_hash: &str) -> Result<(), DownloaderError> {
        delegate!(self, resume_task(info_hash))
    }

    fn get_speed_limits(&self) -> Result<(u64, u64), DownloaderError> {
        delegate!(self, get_speed_limits())
    }

    fn set_speed_limits(
        &self,
        download_limit: u64,
        upload_limit: u64,
    ) -> Result<(), DownloaderError> {
        delegate!(self, set_speed_limits(download_limit, upload_limit))
    }

    fn task_snapshots(&self) -> Result<Vec<downloader::TaskSnapshot>, DownloaderError> {
        delegate!(self, task_snapshots())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{SiteId, Torrent};

    #[test]
    fn dynamic_downloader_delegates_add_with_options_with_save_path() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(Store::open(tmp.path().join("data")).unwrap()));
        let dynamic = DynamicDownloader::new(store, DownloaderEnv::default(), tmp.path());

        let t = Torrent {
            site_id: SiteId::new(),
            title: "Test".into(),
            enclosure: "magnet:?xt=urn:btih:test".into(),
            size_bytes: Some(100),
            seeders: Some(1),
            free: true,
            hr: false,
            imdb_id: None,
            id: None,
            leechers: None,
            snatched: None,
            upload_time: None,
            detail_url: None,
            category: None,
            poster_url: None,
        };

        assert!(
            dynamic
                .add_with_options(&t, &t.enclosure, Some("/custom/path"))
                .is_err(),
            "未配置生产 Downloader 时不得写入内存暂存"
        );
        assert!(dynamic.memory.destinations().is_empty());
    }
}
