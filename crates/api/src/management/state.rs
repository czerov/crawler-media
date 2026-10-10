use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Weak};

use downloader::Downloader;
use indexer::{Fetcher, Indexer, ProfileSet};
use parking_lot::Mutex;

use crate::Store;
use crate::catalog::{Catalog, EmptyCatalog};
use crate::poster_fetch::{HttpPoster, PosterFetch};
use crate::store::StoreError;
use crate::system_logs::{DEFAULT_CAPACITY, LogBuffer};
use domain::{SubscribeId, UserId};

#[derive(Clone)]
pub struct ApiState {
    pub(crate) store: Arc<Mutex<Store>>,
    pub(crate) library_work: Arc<crate::library_lifecycle::LibraryWorkRegistry>,
    pub(crate) jobs: Arc<Mutex<jobs::Queue>>,
    /// Job attempts still executing in this process; protects them from timeout recovery.
    pub(crate) active_job_attempts: Arc<Mutex<HashMap<domain::JobId, jobs::Job>>>,
    /// Serializes a Subscribe pause/update against a worker's final add step.
    subscribe_guards: Arc<Mutex<HashMap<SubscribeId, Weak<Mutex<()>>>>>,
    /// Deletion reservations prevent workers and manual triggers from starting
    /// new work while asynchronous cleanup runs without holding a sync guard.
    deleting_subscribes: Arc<Mutex<HashSet<SubscribeId>>>,
    pub(crate) jellyfin_server_id: Arc<String>,
    pub(crate) indexer: Arc<Indexer>,
    pub(crate) downloader: Arc<dyn Downloader>,
    pub(crate) library_root: Arc<PathBuf>,
    pub(crate) catalog: Arc<dyn Catalog>,
    pub(crate) poster_fetch: Arc<dyn PosterFetch>,
    pub(crate) media_posters: Arc<dyn crate::media_posters::MediaPosterSource>,
    pub log_buffer: LogBuffer,
    pub obscura: Arc<crate::obscura_manager::ObscuraManager>,
    /// 异步媒体探测队列（streamdetails + 声纹指纹后台采集）。
    pub probe: Arc<crate::probe_manager::ProbeManager>,
}

fn apply_admin_bootstrap(
    store: &Store,
    action: crate::bootstrap_credentials::AdminBootstrapAction,
) -> Result<(), StoreError> {
    let user_id = UserId::from_str(crate::bootstrap_credentials::STATIC_ADMIN_UUID_STR)
        .map_err(StoreError::from)?;
    match action {
        crate::bootstrap_credentials::AdminBootstrapAction::Seed {
            password,
            cli_token,
        } => {
            store.seed_admin_with_credentials(user_id, &password, &cli_token)?;
        }
        crate::bootstrap_credentials::AdminBootstrapAction::Rotate {
            password,
            cli_token,
        } => {
            tracing::info!("检测到旧实例管理员口令与 CLI token 相同，安全更新为独立密码");
            store.rotate_admin_password_keeping_cli_token(user_id, &password, &cli_token)?;
        }
        crate::bootstrap_credentials::AdminBootstrapAction::Keep { cli_token } => {
            let _ = store.force_admin_role(user_id);
            store.register_current_cli_token(user_id, &cli_token)?;
        }
    }
    Ok(())
}

impl ApiState {
    pub fn new<F>(
        store: Store,
        token: String,
        profiles: ProfileSet,
        fetcher: Arc<F>,
        downloader: Arc<dyn Downloader>,
        library_root: PathBuf,
    ) -> Result<Self, ApiStateError>
    where
        F: Fetcher + 'static,
    {
        let store = Arc::new(Mutex::new(store));
        Self::new_arc(store, token, profiles, fetcher, downloader, library_root)
    }

    /// Variant that takes the store already wrapped in Arc<Mutex<>>, so
    /// callers can share it with runtime-configurable clients (e.g. TMDB).
    pub fn new_arc<F>(
        store: Arc<Mutex<Store>>,
        token: String,
        profiles: ProfileSet,
        fetcher: Arc<F>,
        downloader: Arc<dyn Downloader>,
        library_root: PathBuf,
    ) -> Result<Self, ApiStateError>
    where
        F: Fetcher + 'static,
    {
        Self::new_arc_with_admin_password(
            store,
            token,
            None,
            profiles,
            fetcher,
            downloader,
            library_root,
        )
    }

    pub fn new_arc_with_admin_password<F>(
        store: Arc<Mutex<Store>>,
        token: String,
        admin_password: Option<String>,
        profiles: ProfileSet,
        fetcher: Arc<F>,
        downloader: Arc<dyn Downloader>,
        library_root: PathBuf,
    ) -> Result<Self, ApiStateError>
    where
        F: Fetcher + 'static,
    {
        let mode = if admin_password.is_some() {
            crate::bootstrap_credentials::BootstrapMode::Production
        } else {
            crate::bootstrap_credentials::BootstrapMode::TestFixture
        };
        let action = crate::bootstrap_credentials::prepare_admin_credentials(
            &store.lock(),
            &token,
            admin_password.as_deref(),
            mode,
        )
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;

        Self::new_arc_with_admin_bootstrap(
            store,
            profiles,
            fetcher,
            downloader,
            library_root,
            action,
        )
    }

    pub fn new_arc_with_admin_bootstrap<F>(
        store: Arc<Mutex<Store>>,
        profiles: ProfileSet,
        fetcher: Arc<F>,
        downloader: Arc<dyn Downloader>,
        library_root: PathBuf,
        action: crate::bootstrap_credentials::AdminBootstrapAction,
    ) -> Result<Self, ApiStateError>
    where
        F: Fetcher + 'static,
    {
        std::fs::create_dir_all(&library_root)?;
        apply_admin_bootstrap(&store.lock(), action)?;
        let jellyfin_server_id = {
            let guard = store.lock();
            match guard.get_setting("jellyfin.server_id")? {
                Some(id) => id,
                None => {
                    let id = uuid::Uuid::new_v4().simple().to_string();
                    guard.put_setting("jellyfin.server_id", &id)?;
                    id
                }
            }
        };
        let jobs = jobs::Queue::open(store.lock().data_dir().join("jobs.db"))
            .map_err(|err| std::io::Error::other(err.to_string()))?;
        crate::jobs_api::seed_instance_defs(&jobs)
            .map_err(|err| std::io::Error::other(err.to_string()))?;
        let data_dir = store.lock().data_dir().to_path_buf();
        // 异步探测队列：ApiState 创建时启动 worker（生产在 Tokio main 内，
        // 有 runtime 才真正 spawn；测试路径仅创建队列不启动）。
        let library_work = Arc::new(crate::library_lifecycle::LibraryWorkRegistry::default());
        let probe = Arc::new(crate::probe_manager::ProbeManager::new(store.clone()));
        probe.set_library_registry(library_work.clone());
        let state = Self {
            store: store.clone(),
            library_work,
            jobs: Arc::new(Mutex::new(jobs)),
            active_job_attempts: Arc::new(Mutex::new(HashMap::new())),
            subscribe_guards: Arc::new(Mutex::new(HashMap::new())),
            deleting_subscribes: Arc::new(Mutex::new(HashSet::new())),
            jellyfin_server_id: Arc::new(jellyfin_server_id),
            indexer: Arc::new(Indexer::new(profiles, fetcher)),
            downloader,
            library_root: Arc::new(library_root),
            catalog: Arc::new(EmptyCatalog),
            poster_fetch: Arc::new(HttpPoster),
            media_posters: Arc::new(crate::media_posters::service::MediaPosterService::new(
                store.clone(),
                Arc::new(EmptyCatalog),
            )),
            log_buffer: LogBuffer::new(DEFAULT_CAPACITY),
            obscura: Arc::new(crate::obscura_manager::ObscuraManager::new(&data_dir)),
            probe,
        };
        if tokio::runtime::Handle::try_current().is_ok() {
            crate::library_lifecycle::resume_library_removals(&state)
                .map_err(std::io::Error::other)?;
            state.probe.try_start_workers();
        }
        Ok(state)
    }

    pub fn with_log_buffer(mut self, log_buffer: LogBuffer) -> Self {
        self.log_buffer = log_buffer;
        self
    }

    pub fn jellyfin_server_id(&self) -> &str {
        self.jellyfin_server_id.as_str()
    }

    pub fn store(&self) -> Arc<Mutex<Store>> {
        self.store.clone()
    }

    pub fn jobs(&self) -> Arc<Mutex<jobs::Queue>> {
        self.jobs.clone()
    }

    pub fn subscribe_guard(&self, id: SubscribeId) -> Arc<Mutex<()>> {
        let mut guards = self.subscribe_guards.lock();
        guards.retain(|_, guard| guard.strong_count() > 0);
        if let Some(guard) = guards.get(&id).and_then(Weak::upgrade) {
            return guard;
        }
        let guard = Arc::new(Mutex::new(()));
        guards.insert(id, Arc::downgrade(&guard));
        guard
    }

    /// Deletion state is read and written while holding `subscribe_guard(id)`.
    pub fn mark_subscribe_deleting(&self, id: SubscribeId) {
        self.deleting_subscribes.lock().insert(id);
    }

    pub fn subscribe_is_deleting(&self, id: SubscribeId) -> bool {
        self.deleting_subscribes.lock().contains(&id)
    }

    pub fn clear_subscribe_deleting(&self, id: SubscribeId) {
        self.deleting_subscribes.lock().remove(&id);
    }

    pub fn with_catalog(mut self, catalog: Arc<dyn Catalog>) -> Self {
        self.catalog = catalog.clone();
        self.media_posters = Arc::new(crate::media_posters::service::MediaPosterService::new(
            self.store.clone(),
            catalog,
        ));
        self
    }

    pub fn with_media_posters(mut self, media_posters: Arc<dyn crate::media_posters::MediaPosterSource>) -> Self {
        self.media_posters = media_posters;
        self
    }

    pub fn with_poster_fetch(mut self, poster_fetch: Arc<dyn PosterFetch>) -> Self {
        self.poster_fetch = poster_fetch;
        self
    }
}

/// Keeps a subscription marked as deleting across async cleanup awaits. Drop
/// clears the reservation after the caller has either deleted the row or
/// abandoned cleanup, so a failed/cancelled request does not strand it.
pub(crate) struct SubscribeDeletionReservation {
    state: ApiState,
    id: SubscribeId,
}

impl SubscribeDeletionReservation {
    pub(crate) fn new(state: ApiState, id: SubscribeId) -> Self {
        Self { state, id }
    }
}

impl Drop for SubscribeDeletionReservation {
    fn drop(&mut self) {
        let guard = self.state.subscribe_guard(self.id);
        let _guard = guard.lock();
        self.state.clear_subscribe_deleting(self.id);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiStateError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
}
