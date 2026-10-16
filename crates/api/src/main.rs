use std::net::SocketAddr;
use std::sync::Arc;

use api::anilist_http::AnilistHttp;
use api::bangumi_http::BangumiHttp;
use api::douban_http::DoubanHttp;
use api::tmdb_http::TmdbHttp;
use api::tvdb_http::TvdbHttp;
use api::{
    ApiState, DownloaderEnv, DynamicDownloader, HttpFetcher, ServerConfig, Store,
    catalog::{
        AnilistCatalog, BangumiCatalog, DoubanCatalog, FanoutCatalog, TmdbCatalog, TvdbCatalog,
    },
    cli, router, spawn_job_loop,
};
use downloader::Downloader;
use indexer::ProfileSet;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match cli::Command::parse(&args) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    if command != cli::Command::Serve {
        let transport = match cli::HttpTransport::from_env() {
            Ok(transport) => transport,
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(2);
            }
        };
        match cli::run(command, &transport).await {
            Ok(out) => print!("{out}"),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if let Err(error) = serve().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn init_logging(
    logs_dir: &std::path::Path,
) -> (
    api::system_logs::LogBuffer,
    tracing_appender::non_blocking::WorkerGuard,
) {
    let log_buffer = api::system_logs::LogBuffer::new(api::system_logs::DEFAULT_CAPACITY);
    let buffer_layer = api::system_logs::LogBufferLayer::new(log_buffer.clone());
    let file_appender = tracing_appender::rolling::daily(logs_dir, "crawler-media.log");
    let (non_blocking_file, guard) = tracing_appender::non_blocking(file_appender);

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new(
                "info,api=debug,crawler_media=debug,domain=debug,indexer=debug,media=debug,\
                 downloader=debug,library=debug,subscribe=debug,filter=debug,release=debug,\
                 hooks=debug,jobs=debug,playback=debug",
            )
        })
        .add_directive("html5ever=off".parse().unwrap())
        .add_directive("selectors=warn".parse().unwrap());

    let timer = api::compact_time::CompactTime;
    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_timer(timer))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking_file)
                .with_ansi(false)
                .with_timer(timer),
        )
        .with(buffer_layer)
        .init();
    (log_buffer, guard)
}

fn seed_store_settings(store: &parking_lot::Mutex<Store>, config: &ServerConfig) {
    if let Some(key) = &config.tmdb_key {
        let _ = store
            .lock()
            .put_setting(api::settings_keys::TMDB_API_KEY, key);
    }
    if let Some(proxy) = &config.metadata_proxy {
        let _ = store
            .lock()
            .put_setting(api::settings_keys::PROXY_METADATA, proxy);
    }
    if let Some(user) = &config.metadata_proxy_user {
        let _ = store
            .lock()
            .put_setting(api::settings_keys::PROXY_USERNAME, user);
    }
    if let Some(pass) = &config.metadata_proxy_pass {
        let _ = store
            .lock()
            .put_setting(api::settings_keys::PROXY_PASSWORD, pass);
    }
    if store
        .lock()
        .get_setting(api::settings_keys::CDP_ENABLED)
        .ok()
        .flatten()
        .is_none()
    {
        let val = if config.browser_enabled { "1" } else { "0" };
        let _ = store
            .lock()
            .put_setting(api::settings_keys::CDP_ENABLED, val);
    }
    api::http_agent::sync_from_store(&store.lock());
    api::user_agent::sync_user_agent(&store.lock());
}

fn load_catalog_sources(
    store: &std::sync::Arc<parking_lot::Mutex<Store>>,
    catalog_db: &std::path::Path,
    tvdb_key: Option<&str>,
) -> Vec<Arc<dyn api::catalog::Catalog>> {
    let mut sources: Vec<Arc<dyn api::catalog::Catalog>> = Vec::new();
    let metadata_language = store
        .lock()
        .get_setting(api::settings_keys::METADATA_LANGUAGE)
        .ok()
        .flatten()
        .unwrap_or_default();
    match media::Tmdb::new(TmdbHttp::new(store.clone()), catalog_db) {
        Ok(tmdb) => sources.push(TmdbCatalog::new(tmdb.with_language(&metadata_language))),
        Err(error) => tracing::error!(%error, "跳过 TMDB：无法打开 catalog 缓存"),
    }
    match media::Douban::new(DoubanHttp, catalog_db) {
        Ok(douban) => sources.push(DoubanCatalog::new(douban)),
        Err(error) => tracing::error!(%error, "跳过豆瓣：无法打开 catalog 缓存"),
    }
    let effective_tvdb_key = tvdb_key
        .map(|k| k.to_string())
        .or_else(|| {
            store
                .lock()
                .get_setting(api::settings_keys::TVDB_API_KEY)
                .ok()
                .flatten()
        })
        .filter(|k| !k.trim().is_empty());
    if let Some(key) = effective_tvdb_key {
        match media::Tvdb::new(TvdbHttp::new(key), catalog_db) {
            Ok(tvdb) => sources.push(TvdbCatalog::new(tvdb)),
            Err(error) => tracing::error!(%error, "跳过 TVDB：无法打开 catalog 缓存"),
        }
    }
    match media::Bangumi::new(BangumiHttp, catalog_db) {
        Ok(bangumi) => sources.push(BangumiCatalog::new(bangumi)),
        Err(error) => tracing::error!(%error, "跳过 Bangumi：无法打开 catalog 缓存"),
    }
    match media::Anilist::new(AnilistHttp, catalog_db) {
        Ok(anilist) => sources.push(AnilistCatalog::new(anilist)),
        Err(error) => tracing::error!(%error, "跳过 AniList：无法打开 catalog 缓存"),
    }
    sources
}

fn spawn_background_services(state: &ApiState, address: SocketAddr) {
    tokio::spawn(api::ssdp::run(
        address.port(),
        state.jellyfin_server_id().to_string(),
    ));
    let _jobs = spawn_job_loop(state.clone());
    api::spawn_fs_watcher(state.clone());
}

fn create_api_state(
    config: &ServerConfig,
    data_dir: &std::path::Path,
    overlay_dir: &std::path::Path,
    store: std::sync::Arc<parking_lot::Mutex<Store>>,
    action: api::bootstrap_credentials::AdminBootstrapAction,
    log_buffer: api::system_logs::LogBuffer,
) -> Result<ApiState, Box<dyn std::error::Error>> {
    let downloader: Arc<dyn Downloader> = Arc::new(DynamicDownloader::new(
        store.clone(),
        DownloaderEnv::from_server_config(config),
        data_dir,
    ));
    let browser = api::runtime_browser::production_browser(store.clone(), config.browser_enabled)?;
    let mut state = ApiState::new_arc_with_admin_bootstrap(
        store.clone(),
        ProfileSet::load(Some(overlay_dir))?,
        Arc::new(HttpFetcher),
        downloader,
        data_dir.join("library"),
        action,
    )?
    .with_log_buffer(log_buffer)
    .with_browser(Arc::new(browser));

    let sources = load_catalog_sources(
        &store,
        &data_dir.join("catalog.db"),
        config.tvdb_key.as_deref(),
    );
    if !sources.is_empty() {
        state = state.with_catalog(FanoutCatalog::new(sources));
    }
    Ok(state)
}

async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    // 先只按 env 定位数据目录并起日志，再解析完整配置：这样 `from_env` 里
    // 未识别变量的 warning 才经过 tracing，而不是丢在没有 subscriber 的空档里。
    let data_dir = api::config::data_dir_from_env();
    let logs_dir = data_dir.join("logs");
    std::fs::create_dir_all(&logs_dir)?;

    let (log_buffer, _guard) = init_logging(&logs_dir);
    let config = ServerConfig::from_env();
    tracing::info!("crawler-media 服务启动中...");

    let overlay_dir = data_dir.join("indexers");
    std::fs::create_dir_all(&overlay_dir)?;

    let token = config.token.clone().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "required environment variable CRAWLER_MEDIA_TOKEN is not set",
        )
    })?;

    let store = std::sync::Arc::new(parking_lot::Mutex::new(Store::open(&data_dir)?));
    seed_store_settings(&store, &config);

    let action = api::bootstrap_credentials::prepare_admin_credentials(
        &store.lock(),
        &token,
        config.admin_password.as_deref(),
        api::bootstrap_credentials::BootstrapMode::Production,
    )
    .map_err(|e| {
        tracing::error!(error = %e, "管理员凭据校验未通过，服务拒绝启动");
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            e.to_string(),
        )) as Box<dyn std::error::Error>
    })?;

    let state = create_api_state(
        &config,
        &data_dir,
        &overlay_dir,
        store.clone(),
        action,
        log_buffer,
    )?;

    let address: SocketAddr = config.listen.parse()?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    println!("crawler-media 已监听 http://{address}");

    spawn_background_services(&state, address);

    let app = api::ui::attach_ui(router(state), config.ui_dir.clone());
    axum::serve(listener, app).await?;
    Ok(())
}
