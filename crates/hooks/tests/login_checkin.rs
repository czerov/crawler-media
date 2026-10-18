use std::sync::{Arc, Mutex};
use std::time::Duration;

use domain::{Site, SiteId};
use hooks::{Bus, CheckInPlugin, Hook, HookEvent, LoginPlugin, PluginError, SiteCredentials, Step};
use indexer::{Browser, BrowserConfig, IndexerError, PageSession};

fn site() -> Site {
    Site {
        id: SiteId::new(),
        name: "demo".into(),
        url: "https://pt.example/".into(),
        profile_id: "demo".into(),
        cookie: Some("expired=1".into()),
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: Some("http://127.0.0.1:9222".into()),
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
    }
}

struct MemoryStore {
    site: Mutex<Site>,
}

impl SiteCredentials for MemoryStore {
    fn load(&self, id: SiteId) -> Result<Site, PluginError> {
        let site = self.site.lock().unwrap();
        if site.id == id {
            Ok(site.clone())
        } else {
            Err(PluginError::NotFound)
        }
    }

    fn save(&self, site: &Site) -> Result<(), PluginError> {
        *self.site.lock().unwrap() = site.clone();
        Ok(())
    }
}

struct FakePage {
    navigated: Mutex<Vec<String>>,
    read_cookies: Option<String>,
}

impl PageSession for FakePage {
    fn goto(&self, url: &str) -> Result<(), IndexerError> {
        self.navigated.lock().unwrap().push(url.to_string());
        Ok(())
    }

    fn cookie_header(&self, _url: &str) -> Result<String, IndexerError> {
        Ok(self.read_cookies.clone().unwrap_or_default())
    }

    fn set_cookie_header(&self, _header: &str) -> Result<(), IndexerError> {
        Ok(())
    }

    fn content(&self) -> Result<String, IndexerError> {
        Ok(r#"<html><body><p>签到成功</p><a href="logout.php">Logout</a></body></html>"#.into())
    }
}

#[derive(Default)]
struct RecordingHttp {
    posts: Mutex<Vec<String>>,
    proxies: Mutex<Vec<Option<String>>>,
}

impl hooks::HttpPost for RecordingHttp {
    fn get(&self, url: &str, _: Option<&str>, proxy: Option<&str>) -> Result<String, PluginError> {
        self.posts.lock().unwrap().push(url.to_string());
        self.proxies.lock().unwrap().push(proxy.map(str::to_string));
        Ok("Set-Cookie: uid=fresh\n<html><a href='logout.php'>Logout</a></html>".into())
    }

    fn post(
        &self,
        url: &str,
        _body: &str,
        _cookie: Option<&str>,
        proxy: Option<&str>,
    ) -> Result<String, PluginError> {
        self.posts.lock().unwrap().push(url.to_string());
        self.proxies.lock().unwrap().push(proxy.map(str::to_string));
        Ok("Set-Cookie: uid=fresh\n<html>签到成功</html>".into())
    }
}

#[test]
fn login_plugin_refreshes_cookie_via_http_and_persists() {
    let store = MemoryStore {
        site: Mutex::new(site()),
    };
    let http = RecordingHttp::default();
    let bus = Bus::new();
    let plugin = LoginPlugin::http(&bus, &store, &http);
    let id = store.site.lock().unwrap().id;

    plugin.login(id).unwrap();

    assert_eq!(
        store.site.lock().unwrap().cookie.as_deref(),
        Some("expired=1; uid=fresh")
    );
    assert_eq!(
        http.posts.lock().unwrap().as_slice(),
        &["https://pt.example/index.php".to_string()]
    );
}

#[test]
fn login_plugin_can_use_browser_session() {
    let store = MemoryStore {
        site: Mutex::new(site()),
    };
    let page = Arc::new(FakePage {
        navigated: Mutex::new(Vec::new()),
        read_cookies: Some("uid=fresh_browser; pass=xyz".into()),
    });
    let page_for_open = page.clone();
    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
        Ok(page_for_open.clone() as Arc<dyn PageSession>)
    });
    let bus = Bus::new();
    let plugin = LoginPlugin::browser(&bus, &store, &browser);
    let id = store.site.lock().unwrap().id;

    plugin.login(id).unwrap();

    assert_eq!(
        *page.navigated.lock().unwrap(),
        vec!["https://pt.example/index.php".to_string()]
    );
    assert_eq!(
        store.site.lock().unwrap().cookie.as_deref(),
        Some("expired=1; pass=xyz; uid=fresh_browser")
    );
}

#[test]
fn check_in_plugin_can_use_browser_session() {
    let store = MemoryStore {
        site: Mutex::new(site()),
    };
    let page = Arc::new(FakePage {
        navigated: Mutex::new(Vec::new()),
        read_cookies: Some("attendance=checked_in".into()),
    });
    let page_for_open = page.clone();
    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
        Ok(page_for_open.clone() as Arc<dyn PageSession>)
    });
    let bus = Bus::new();
    let plugin = CheckInPlugin::browser(&bus, &store, &browser);
    let id = store.site.lock().unwrap().id;

    plugin.check_in(id).unwrap();

    assert_eq!(
        *page.navigated.lock().unwrap(),
        vec!["https://pt.example/attendance.php".to_string()]
    );
    assert_eq!(
        store.site.lock().unwrap().cookie.as_deref(),
        Some("attendance=checked_in; expired=1")
    );
}

#[test]
fn check_in_plugin_runs_on_schedule_without_searching() {
    let store = MemoryStore {
        site: Mutex::new(site()),
    };
    let searched = Arc::new(Mutex::new(false));
    let flag = searched.clone();
    let bus = Bus::new();
    bus.register(Hook::new(Step::CheckIn, move |_event: &HookEvent| {
        *flag.lock().unwrap() = false;
        Ok(())
    }));
    let http = RecordingHttp::default();
    let plugin = CheckInPlugin::http(&bus, &store, &http);
    let id = store.site.lock().unwrap().id;

    plugin.check_in(id).unwrap();
    assert!(!*searched.lock().unwrap());
    plugin.tick(Duration::from_secs(24 * 60 * 60), id).unwrap();
}

#[test]
fn check_in_plugin_posts_attendance_and_persists_cookie() {
    let store = MemoryStore {
        site: Mutex::new(site()),
    };
    let http = RecordingHttp::default();
    let bus = Bus::new();
    let plugin = CheckInPlugin::http(&bus, &store, &http);
    let id = store.site.lock().unwrap().id;

    plugin.check_in(id).unwrap();

    assert_eq!(
        store.site.lock().unwrap().cookie.as_deref(),
        Some("expired=1; uid=fresh")
    );
    assert_eq!(
        http.posts.lock().unwrap().as_slice(),
        &["https://pt.example/attendance.php".to_string()]
    );
}

#[test]
fn veto_hook_prevents_check_in_post() {
    let store = MemoryStore {
        site: Mutex::new(site()),
    };
    let http = RecordingHttp::default();
    let bus = Bus::new();
    bus.register(Hook::new(Step::CheckIn, |_event: &HookEvent| {
        Err(PluginError::Veto("no".into()))
    }));
    let plugin = CheckInPlugin::http(&bus, &store, &http);
    let id = store.site.lock().unwrap().id;

    let err = plugin.check_in(id).unwrap_err();
    assert!(matches!(err, PluginError::Veto(_)));
    assert!(http.posts.lock().unwrap().is_empty());
    assert_eq!(
        store.site.lock().unwrap().cookie.as_deref(),
        Some("expired=1")
    );
}

#[test]
fn veto_hook_prevents_login_side_effect() {
    let store = MemoryStore {
        site: Mutex::new(site()),
    };
    let http = RecordingHttp::default();
    let bus = Bus::new();
    bus.register(Hook::new(Step::Login, |_| {
        Err(PluginError::Veto("skip".into()))
    }));
    let plugin = LoginPlugin::http(&bus, &store, &http);
    let id = store.site.lock().unwrap().id;
    let err = plugin.login(id).unwrap_err();
    assert!(matches!(err, PluginError::Veto(_)));
    assert_eq!(
        store.site.lock().unwrap().cookie.as_deref(),
        Some("expired=1")
    );
    assert!(http.posts.lock().unwrap().is_empty());
}

#[test]
fn keep_site_alive_requires_cookie_when_login_credentials_are_unavailable() {
    let mut row = site();
    row.cookie = None;
    let store = MemoryStore {
        site: Mutex::new(row),
    };
    let http = RecordingHttp::default();
    let bus = Bus::new();
    let id = store.site.lock().unwrap().id;
    let error = hooks::keep_site_alive(&bus, &store, &http, id).unwrap_err();
    assert!(error.to_string().contains("Cookie"));
    assert!(http.posts.lock().unwrap().is_empty());
    assert!(store.site.lock().unwrap().cookie.is_none());
}

#[test]
fn keep_site_alive_skips_login_when_cookie_present() {
    let store = MemoryStore {
        site: Mutex::new(site()),
    };
    let http = RecordingHttp::default();
    let bus = Bus::new();
    let id = store.site.lock().unwrap().id;
    hooks::keep_site_alive(&bus, &store, &http, id).unwrap();
    assert_eq!(
        http.posts.lock().unwrap().as_slice(),
        &["https://pt.example/attendance.php".to_string()]
    );
}

#[test]
fn check_in_passes_site_proxy_to_http_post() {
    let mut row = site();
    row.proxy = Some("http://proxy.example:8080".into());
    let store = MemoryStore {
        site: Mutex::new(row),
    };
    let http = RecordingHttp::default();
    let bus = Bus::new();
    let plugin = CheckInPlugin::http(&bus, &store, &http);
    let id = store.site.lock().unwrap().id;

    plugin.check_in(id).unwrap();

    let proxies = http.proxies.lock().unwrap().clone();
    assert_eq!(
        proxies.as_slice(),
        &[Some("http://proxy.example:8080".to_string())],
        "站点签到必须如实传递站点专属代理设置，绝不可静默丢失"
    );
}
