//! Resolve Torrent download URLs and keep each pending task on its Downloader.

use std::collections::HashMap;
use std::sync::Arc;

use domain::{DownloaderId, Subscribe, Torrent};
use downloader::{Downloader, DownloaderError, QbitDownloader, TransmissionDownloader};
use parking_lot::Mutex;

use crate::management::ApiState;
use crate::runtime_downloader::{ChosenDownloader, DownloaderEnv, row_to_chosen};

fn error(message: impl ToString) -> DownloaderError {
    DownloaderError::Message(message.to_string())
}

/// A persisted id is used only while it remains enabled for new deliveries.
/// Existing pending tasks can still use a Downloader that was later disabled.
fn enabled_id(
    store: &crate::Store,
    id: DownloaderId,
) -> Result<Option<DownloaderId>, DownloaderError> {
    match store.get_downloader(id).map_err(error)? {
        Some(row) if row.enabled => Ok(Some(id)),
        _ => {
            tracing::warn!(%id, "配置的下载器不可用，改用下一个符合条件的下载目标");
            Ok(None)
        }
    }
}

pub(crate) fn target_id(
    state: &ApiState,
    subscribe: Option<&Subscribe>,
    torrent: &Torrent,
) -> Result<Option<DownloaderId>, DownloaderError> {
    let store = state.store.lock();
    if let Some(id) = subscribe.and_then(|sub| sub.downloader_id) {
        if let Some(id) = enabled_id(&store, id)? {
            return Ok(Some(id));
        }
    }
    if let Some(id) = store
        .get_site(torrent.site_id)
        .map_err(error)?
        .and_then(|site| site.downloader_id)
    {
        if let Some(id) = enabled_id(&store, id)? {
            return Ok(Some(id));
        }
    }
    Ok(store.default_downloader().map_err(error)?.map(|row| row.id))
}

pub(crate) fn client_for_id(
    state: &ApiState,
    id: Option<DownloaderId>,
) -> Result<Arc<dyn Downloader>, DownloaderError> {
    let Some(id) = id else {
        return Ok(state.downloader.clone());
    };
    let row = {
        let store = state.store.lock();
        if store
            .default_downloader()
            .map_err(error)?
            .is_some_and(|row| row.id == id)
        {
            return Ok(state.downloader.clone());
        }
        store
            .get_downloader(id)
            .map_err(error)?
            .ok_or_else(|| error(format!("Downloader {id} no longer exists")))?
    };
    match row_to_chosen(row, &DownloaderEnv::default()) {
        ChosenDownloader::Qbittorrent(config) => {
            QbitDownloader::connect(config).map(|client| Arc::new(client) as Arc<dyn Downloader>)
        }
        ChosenDownloader::Transmission(config) => TransmissionDownloader::connect(config)
            .map(|client| Arc::new(client) as Arc<dyn Downloader>),
        ChosenDownloader::Memory | ChosenDownloader::Unavailable => {
            Err(error(format!("Downloader {id} is incomplete")))
        }
    }
}

/// Concrete client for a route, frozen at this moment.
///
/// The default route may be a runtime proxy that re-selects configuration on
/// every call. Cleanup must snapshot the underlying connection so a later
/// switch cannot retarget a planned delete.
pub(crate) fn frozen_client_for_id(
    state: &ApiState,
    id: Option<DownloaderId>,
) -> Result<Arc<dyn Downloader>, DownloaderError> {
    let client = client_for_id(state, id)?;
    Ok(client.snapshot_client()?.unwrap_or(client))
}

/// Used at the public manual-submit seam and by the per-Subscribe adapter.
pub(crate) fn submit_torrent(
    state: &ApiState,
    torrent: &Torrent,
    id: Option<DownloaderId>,
) -> Result<(), DownloaderError> {
    submit_torrent_with_options(state, torrent, id, None)
}

pub(crate) fn submit_torrent_with_options(
    state: &ApiState,
    torrent: &Torrent,
    id: Option<DownloaderId>,
    save_path: Option<&str>,
) -> Result<(), DownloaderError> {
    let client = client_for_id(state, id)?;
    submit_on(state, client.as_ref(), torrent, save_path)
}

/// Submit `torrent` on an already resolved client (keeps its in-memory identity).
fn submit_on(
    state: &ApiState,
    client: &dyn Downloader,
    torrent: &Torrent,
    save_path: Option<&str>,
) -> Result<(), DownloaderError> {
    let site = state
        .store
        .lock()
        .get_site(torrent.site_id)
        .map_err(error)?;
    let resolved_url = site
        .as_ref()
        .map(|site| state.indexer.resolve_torrent_download(site, torrent))
        .transpose()
        .map_err(error)?
        .flatten();
    match resolved_url {
        Some(url) => client.add_with_options(torrent, &url, save_path),
        None => client.add_with_options(torrent, &torrent.enclosure, save_path),
    }
}

pub(crate) struct RoutedDownloader<'a> {
    state: &'a ApiState,
    subscribe: Option<&'a Subscribe>,
    pending: HashMap<String, Option<DownloaderId>>,
    used: Mutex<HashMap<String, Option<DownloaderId>>>,
    clients: Mutex<HashMap<Option<DownloaderId>, Arc<dyn Downloader>>>,
    /// D-M3: info hash reported by the client right after each submission.
    hashes: Mutex<HashMap<String, String>>,
    /// G-M2: torrents whose existing client task turned out broken.
    broken: Mutex<Vec<Torrent>>,
}

impl<'a> RoutedDownloader<'a> {
    pub(crate) fn new(state: &'a ApiState, subscribe: Option<&'a Subscribe>) -> Self {
        Self {
            state,
            subscribe,
            pending: HashMap::new(),
            used: Mutex::new(HashMap::new()),
            clients: Mutex::new(HashMap::new()),
            hashes: Mutex::new(HashMap::new()),
            broken: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn with_pending(
        state: &'a ApiState,
        subscribe: &'a Subscribe,
        pending: &[(i32, crate::store::PendingDownload)],
    ) -> Self {
        let targets = pending
            .iter()
            .map(|(_, item)| (item.torrent.enclosure.clone(), item.downloader_id))
            .collect();
        Self {
            state,
            subscribe: Some(subscribe),
            pending: targets,
            used: Mutex::new(HashMap::new()),
            clients: Mutex::new(HashMap::new()),
            hashes: Mutex::new(HashMap::new()),
            broken: Mutex::new(Vec::new()),
        }
    }

    /// D-M3: info hash of a torrent submitted through this router, if known.
    pub(crate) fn submitted_hash(&self, torrent: &Torrent) -> Option<String> {
        self.hashes.lock().get(&torrent.enclosure).cloned()
    }

    /// G-M2: torrents rejected because their existing task is broken.
    pub(crate) fn broken_torrents(&self) -> Vec<Torrent> {
        self.broken.lock().clone()
    }

    pub(crate) fn used_id(&self, torrent: &Torrent) -> Option<DownloaderId> {
        self.used.lock().get(&torrent.enclosure).copied().flatten()
    }

    pub(crate) fn imported_destinations(
        &self,
    ) -> Result<Vec<subscribe::collection_destinations::DestinationMapping>, DownloaderError> {
        let Some(subscribe) = self.subscribe else {
            return Ok(Vec::new());
        };
        let store = self.state.store.lock();
        let media = store
            .get_media(subscribe.media_id)
            .map_err(error)?
            .ok_or_else(|| error("Media not found"))?;
        let library_id = subscribe.library_id.map(|id| id.to_string());
        let (root, _, _) =
            crate::directory::transfer_plan_for_library(&store, media.kind, library_id.as_deref())
                .map_err(error)?;
        let target = store.library_for_path(&root, media.kind).map_err(error)?;
        let mut sources = Vec::new();
        for (row, _, source) in store.list_ledger_with_mode().map_err(error)? {
            if row.media_id != subscribe.media_id {
                continue;
            }
            let path = std::path::Path::new(&row.path);
            let in_target = match &target {
                Some(target) => store
                    .library_for_path(path, media.kind)
                    .map_err(error)?
                    .is_some_and(|owner| owner.id == target.id),
                None => path.starts_with(&root),
            };
            if in_target {
                if let Some(source) = source {
                    let mut file_quality = release::parse(
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or_default(),
                    );
                    if row.resolution.is_some() {
                        file_quality.resolution = row.resolution.clone();
                    }
                    if row.codec.is_some() {
                        file_quality.codec = row.codec.clone();
                    }
                    if row.hdr.is_some() {
                        file_quality.hdr = row.hdr.clone();
                    }
                    let mut slots = Vec::new();
                    if let Some((from, to)) = file_quality.episode_span() {
                        for ep in from..=to {
                            slots.push((file_quality.season, Some(ep)));
                        }
                    }
                    if slots.is_empty() {
                        slots.push((row.season, row.episode));
                    }
                    let mut mapping = subscribe::collection_destinations::DestinationMapping::with_slots_and_quality(
                        source, path, slots, Some(file_quality),
                    );
                    mapping.transfer_pending = store.transfer_pending(&row.path).map_err(error)?;
                    sources.push(mapping);
                }
            }
        }
        Ok(sources)
    }

    fn client_for(&self, torrent: &Torrent) -> Result<Arc<dyn Downloader>, DownloaderError> {
        let id = self
            .pending
            .get(&torrent.enclosure)
            .copied()
            .or_else(|| self.used.lock().get(&torrent.enclosure).copied())
            .unwrap_or(None);
        if let Some(client) = self.clients.lock().get(&id) {
            return Ok(client.clone());
        }
        let client = client_for_id(self.state, id)?;
        self.clients.lock().insert(id, client.clone());
        Ok(client)
    }
}

impl Downloader for RoutedDownloader<'_> {
    fn remember_identity(&self, torrent: &Torrent, hash: &str) -> Result<(), DownloaderError> {
        self.client_for(torrent)?.remember_identity(torrent, hash)
    }

    fn select_files(
        &self,
        torrent: &Torrent,
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<Option<usize>, DownloaderError> {
        self.client_for(torrent)?.select_files(torrent, keep)
    }

    fn add(&self, torrent: &Torrent) -> Result<(), DownloaderError> {
        let id = match self.pending.get(&torrent.enclosure) {
            Some(id) => *id,
            None => target_id(self.state, self.subscribe, torrent)?,
        };
        self.used.lock().insert(torrent.enclosure.clone(), id);
        let client = self.client_for(torrent)?;
        if let Err(error) = submit_on(self.state, client.as_ref(), torrent, None) {
            if matches!(error, DownloaderError::ExistingTaskBroken(..)) {
                self.broken.lock().push(torrent.clone());
            } else {
                self.used.lock().remove(&torrent.enclosure);
            }
            return Err(error);
        }
        match client.owned_identity(torrent) {
            Ok(Some(hash)) => {
                self.hashes.lock().insert(torrent.enclosure.clone(), hash);
            }
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(torrent = %torrent.title, %error, "提交后读取 info hash 失败")
            }
        }
        Ok(())
    }

    fn completed_files(
        &self,
        torrent: &Torrent,
    ) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        // Keep source identities visible to collection. Persisted destinations
        // deduplicate video writes there and identify the matching sidecars.
        self.client_for(torrent)?.completed_files(torrent)
    }

    fn is_claimed(&self, torrent: &Torrent) -> Result<bool, DownloaderError> {
        self.client_for(torrent)?.is_claimed(torrent)
    }

    fn collectable_files(
        &self,
        torrent: &Torrent,
    ) -> Result<(Vec<std::path::PathBuf>, bool), DownloaderError> {
        self.client_for(torrent)?.collectable_files(torrent)
    }

    fn remove(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        self.client_for(torrent)?.remove(torrent, delete_files)
    }

    fn remove_owned(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        self.client_for(torrent)?
            .remove_owned(torrent, delete_files)
    }

    fn owned_identity(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        self.client_for(torrent)?.owned_identity(torrent)
    }

    fn endpoint(&self) -> Option<String> {
        // Route-specific clients differ per Torrent; report the default route.
        match client_for_id(self.state, None) {
            Ok(client) => client.endpoint(),
            Err(error) => {
                tracing::error!(%error, "无法解析默认下载器端点");
                None
            }
        }
    }
}
