//! DB10：解析 / 损坏 / secret-fallback 的统一语义。
//!
//! 纯函数与注入 seam 测试：不修改进程全局 env，不安装全局 subscriber
//! （日志捕获用 `tracing::subscriber::with_default`，只作用于当前线程）。

use std::sync::{Arc, Mutex};

use api::scrape_store::ScrapeStoreExt;
use api::{Store, settings_keys};
use tracing_subscriber::layer::SubscriberExt;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 在**当前线程**捕获 tracing 输出；不影响并行运行的其它测试。
fn capture_logs<F: FnOnce()>(body: F) -> String {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let writer = {
        let buffer = buffer.clone();
        move || Capture(buffer.clone())
    };
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .without_time()
            .with_writer(writer),
    );
    tracing::subscriber::with_default(subscriber, body);
    let captured = buffer.lock().unwrap().clone();
    String::from_utf8(captured).expect("utf-8 logs")
}

#[test]
fn corrupt_scrape_config_logs_error_and_preserves_raw_value() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let raw = r#"{"naming_entry_dir":"Movies","naming_customized":tru"#;
    store
        .put_setting(settings_keys::METADATA_SCRAPE, raw)
        .unwrap();

    let logs = capture_logs(|| {
        let config = store.get_scrape_config().expect("坏 JSON 必须回退而不是报错");
        assert!(
            !config.effective.compose_movie_pattern().is_empty(),
            "回退后必须得到可用的默认配置"
        );
    });

    assert!(
        logs.contains("scrape config JSON is corrupt"),
        "损坏 JSON 必须记录 error 日志: {logs}"
    );
    assert!(logs.contains("ERROR"), "必须是 error 级别: {logs}");
    assert_eq!(
        store
            .get_setting(settings_keys::METADATA_SCRAPE)
            .unwrap()
            .as_deref(),
        Some(raw),
        "坏原文不得被覆盖"
    );
}

#[test]
fn server_config_debug_redacts_secrets() {
    let mut config = api::ServerConfig::from_env();
    config.token = Some("tok-secret-9f8e".into());
    config.admin_password = Some("admin-secret-1234".into());
    config.tmdb_key = Some("tmdb-secret-5678".into());
    config.tvdb_key = Some("tvdb-secret-9012".into());
    config.qb_pass = Some("qb-secret-3456".into());
    config.metadata_proxy_pass = Some("proxy-secret-7890".into());
    config.metadata_proxy = Some("socks5://user:proxy-secret-7890@127.0.0.1:1080".into());

    let rendered = format!("{config:?}");
    for secret in [
        "tok-secret-9f8e",
        "admin-secret-1234",
        "tmdb-secret-5678",
        "tvdb-secret-9012",
        "qb-secret-3456",
        "proxy-secret-7890",
    ] {
        assert!(
            !rendered.contains(secret),
            "Debug 输出泄露了 secret {secret}: {rendered}"
        );
    }
    assert!(
        rendered.contains("data_dir") && rendered.contains("listen"),
        "Debug 仍应输出非敏感字段名: {rendered}"
    );
}

/// 注入式 env 读取 seam：不触碰进程全局 env，因此并行测试之间不会互相污染。
struct MapEnv(std::collections::HashMap<String, String>);

impl MapEnv {
    fn new(pairs: &[(&str, &str)]) -> Self {
        Self(
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        )
    }
}

impl api::config::EnvSource for MapEnv {
    fn value(&self, name: &str) -> Option<String> {
        self.0.get(name).filter(|v| !v.is_empty()).cloned()
    }
}

#[test]
fn bool_vocabulary_is_shared_across_env_kv_and_cli() {
    for raw in ["1", "true", "TRUE", "True", "yes", "YES", "on", "ON", " on "] {
        assert_eq!(api::config::parse_bool(raw), Some(true), "{raw:?} 必须为 true");
    }
    for raw in ["0", "false", "FALSE", "False", "no", "NO", "off", "OFF"] {
        assert_eq!(
            api::config::parse_bool(raw),
            Some(false),
            "{raw:?} 必须为 false"
        );
    }
    for raw in ["", "   ", "maybe", "2", "enable", "ture"] {
        assert_eq!(api::config::parse_bool(raw), None, "{raw:?} 必须无法识别");
    }
}

#[test]
fn env_flag_accepts_on_and_ignores_typos() {
    let on = MapEnv::new(&[("CRAWLER_MEDIA_BROWSER", "on")]);
    assert!(
        api::config::env_flag_from(&on, "CRAWLER_MEDIA_BROWSER"),
        "CRAWLER_MEDIA_BROWSER=on 必须开启"
    );
    let off = MapEnv::new(&[("CRAWLER_MEDIA_BROWSER", "OFF")]);
    assert!(!api::config::env_flag_from(&off, "CRAWLER_MEDIA_BROWSER"));
    let typo = MapEnv::new(&[("CRAWLER_MEDIA_BROWSER", "ture")]);
    assert!(
        !api::config::env_flag_from(&typo, "CRAWLER_MEDIA_BROWSER"),
        "拼错的值不得意外开启功能"
    );
    assert!(!api::config::env_flag_from(
        &MapEnv::new(&[]),
        "CRAWLER_MEDIA_BROWSER"
    ));
}

#[test]
fn cdp_switch_reads_only_new_key_and_defaults_off() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let settings = api::runtime_browser::BrowserSettings::load(&store).unwrap();
    assert_eq!(settings.response()["cdp"]["enabled"], false);

    store.put_setting(settings_keys::CDP_ENABLED, "on").unwrap();
    let settings = api::runtime_browser::BrowserSettings::load(&store).unwrap();
    assert_eq!(settings.response()["cdp"]["enabled"], true);
}

#[test]
fn unknown_new_switch_value_is_an_error_not_http_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.put_setting(settings_keys::CDP_ENABLED, "maybe").unwrap();
    assert!(api::runtime_browser::BrowserSettings::load(&store).is_err());
}

#[test]
fn unknown_obscura_install_policy_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert!(api::runtime_browser::BrowserSettings::save(
        &store,
        &serde_json::json!({
            "obscura": { "install": "invalid_policy" }
        })
    ).is_err());
}

#[test]
fn new_site_defaults_to_http_maintenance() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let site = domain::Site {
        id: domain::SiteId::new(),
        name: "test".into(),
        url: "https://example.com/".into(),
        profile_id: "demo".into(),
        cookie: None,
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
    };
    store.insert_site(&site).unwrap();
    let loaded = store.get_site(site.id).unwrap().unwrap();
    assert_eq!(loaded.maintenance_via, domain::MaintenanceVia::Http);
}

#[test]
fn proxy_env_counts_only_non_empty_values() {
    let empty = MapEnv::new(&[("CRAWLER_MEDIA_METADATA_PROXY", "")]);
    assert_eq!(
        api::config::proxy_env(&empty).url,
        None,
        "空串不算 override"
    );

    let lower = MapEnv::new(&[("http_proxy", "http://127.0.0.1:7890")]);
    let env = api::config::proxy_env(&lower);
    assert_eq!(
        env.url.as_deref(),
        Some("http://127.0.0.1:7890"),
        "小写 http_proxy 与运行时兜底一致"
    );

    let named = MapEnv::new(&[("CRAWLER_MEDIA_METADATA_PROXY", "socks5://127.0.0.1:1080")]);
    assert_eq!(
        api::config::proxy_env(&named).url.as_deref(),
        Some("socks5://127.0.0.1:1080")
    );
}

#[test]
fn proxy_env_resolves_secret_file_variants() {
    let dir = tempfile::tempdir().unwrap();
    let user_path = dir.path().join("user");
    let pass_path = dir.path().join("pass");
    std::fs::write(&user_path, "alice\n").unwrap();
    std::fs::write(&pass_path, " s3cr3t \n").unwrap();
    let source = MapEnv::new(&[
        (
            "CRAWLER_MEDIA_METADATA_PROXY_USER_FILE",
            user_path.to_str().unwrap(),
        ),
        (
            "CRAWLER_MEDIA_METADATA_PROXY_PASS_FILE",
            pass_path.to_str().unwrap(),
        ),
    ]);
    let env = api::config::proxy_env(&source);
    assert_eq!(env.username.as_deref(), Some("alice"));
    assert_eq!(env.password.as_deref(), Some("s3cr3t"));

    let direct = MapEnv::new(&[
        ("CRAWLER_MEDIA_METADATA_PROXY_PASS", "direct"),
        (
            "CRAWLER_MEDIA_METADATA_PROXY_PASS_FILE",
            pass_path.to_str().unwrap(),
        ),
    ]);
    assert_eq!(
        api::config::proxy_env(&direct).password.as_deref(),
        Some("direct"),
        "直接值优先于 _FILE"
    );

    std::fs::write(&pass_path, "\n").unwrap();
    assert_eq!(
        api::config::proxy_env(&source).password,
        None,
        "空 secret 文件不算已配置"
    );
}

#[test]
fn env_whitelist_covers_every_variable_the_server_reads() {
    let used = [
        "CRAWLER_MEDIA_DATA",
        "CRAWLER_MEDIA_TOKEN",
        "CRAWLER_MEDIA_ADMIN_PASSWORD",
        "CRAWLER_MEDIA_LISTEN",
        "CRAWLER_MEDIA_UI",
        "CRAWLER_MEDIA_CORS_ORIGINS",
        "CRAWLER_MEDIA_TMDB_KEY",
        "CRAWLER_MEDIA_TVDB_KEY",
        "CRAWLER_MEDIA_BROWSER",
        "CRAWLER_MEDIA_QB_URL",
        "CRAWLER_MEDIA_QB_USER",
        "CRAWLER_MEDIA_QB_PASS",
        "CRAWLER_MEDIA_QB_CATEGORY",
        "CRAWLER_MEDIA_QB_PATH_MAP",
        "CRAWLER_MEDIA_TR_PATH_MAP",
        "CRAWLER_MEDIA_METADATA_PROXY",
        "CRAWLER_MEDIA_METADATA_PROXY_USER",
        "CRAWLER_MEDIA_METADATA_PROXY_PASS",
        "CRAWLER_MEDIA_URL",
        "CRAWLER_MEDIA_FFMPEG_LOG_LEVEL",
        "CRAWLER_MEDIA_TOKEN_FILE",
        "CRAWLER_MEDIA_ADMIN_PASSWORD_FILE",
        "CRAWLER_MEDIA_TMDB_KEY_FILE",
        "CRAWLER_MEDIA_TVDB_KEY_FILE",
        "CRAWLER_MEDIA_QB_PASS_FILE",
        "CRAWLER_MEDIA_METADATA_PROXY_USER_FILE",
        "CRAWLER_MEDIA_METADATA_PROXY_PASS_FILE",
    ];
    let unknown = api::config::unknown_env_keys(used.iter().map(|key| (*key).to_string()));
    assert!(
        unknown.is_empty(),
        "白名单缺少实际在用的变量，启动会打假 warning: {unknown:?}"
    );
    assert_eq!(
        api::config::unknown_env_keys([
            "CRAWLER_MEDIA_TMDB_KET".to_string(),
            "PATH".to_string()
        ]),
        ["CRAWLER_MEDIA_TMDB_KET".to_string()],
        "只有 CRAWLER_MEDIA_ 前缀的未知名才告警"
    );
}

#[test]
fn env_pin_mapping_matches_the_startup_seed() {
    assert_eq!(
        api::config::env_pin(settings_keys::TMDB_API_KEY),
        Some(("CRAWLER_MEDIA_TMDB_KEY", true))
    );
    assert_eq!(
        api::config::env_pin(settings_keys::TVDB_API_KEY),
        Some(("CRAWLER_MEDIA_TVDB_KEY", true))
    );
    assert_eq!(
        api::config::env_pin(settings_keys::PROXY_METADATA),
        Some(("CRAWLER_MEDIA_METADATA_PROXY", false))
    );
    assert_eq!(
        api::config::env_pin(settings_keys::PROXY_USERNAME),
        Some(("CRAWLER_MEDIA_METADATA_PROXY_USER", false))
    );
    assert_eq!(
        api::config::env_pin(settings_keys::PROXY_PASSWORD),
        Some(("CRAWLER_MEDIA_METADATA_PROXY_PASS", true))
    );
    assert_eq!(
        api::config::env_pin(settings_keys::METADATA_LANGUAGE),
        None,
        "语言没有 env 固定，必须保持可写"
    );
}
