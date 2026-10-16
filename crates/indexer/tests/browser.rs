use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use domain::{Site, SiteId};
use indexer::{
    Browser, BrowserConfig, FetchMethod, FetchRequest, Fetcher, Indexer, IndexerError, PageSession, ProfileSet,
    RecordingFetcher,
};

fn site_with_profile(profile_id: &str, cdp_url: Option<&str>) -> Site {
    Site {
        id: SiteId::new(),
        name: profile_id.into(),
        url: "https://pt.example/".into(),
        profile_id: profile_id.into(),
        cookie: Some("uid=1".into()),
        api_key: None,
        rss_url: Some("https://pt.example/rss".into()),
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: cdp_url.map(str::to_string),
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
    }
}

struct Capture {
    requests: Mutex<Vec<FetchRequest>>,
    html: String,
}

impl Fetcher for Capture {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        self.requests.lock().unwrap().push(FetchRequest {
            key: request.key.clone(),
            url: request.url.clone(),
            method: request.method,
            body: request.body.clone(),
            cookie: request.cookie.clone(),
            api_key: request.api_key.clone(),
            proxy: request.proxy.clone(),
            cdp_url: request.cdp_url.clone(),
        });
        Ok(self.html.clone())
    }
}

struct FakePage {
    html: String,
    cookies: Option<String>,
    navigated: Mutex<Vec<String>>,
}

impl PageSession for FakePage {
    fn goto(&self, url: &str) -> Result<(), IndexerError> {
        self.navigated.lock().unwrap().push(url.to_string());
        Ok(())
    }

    fn set_cookie_header(&self, header: &str) -> Result<(), IndexerError> {
        assert_eq!(header, self.cookies.clone().unwrap_or_default());
        Ok(())
    }

    fn set_site_cookies(&self, _page_url: &str, header: &str) -> Result<(), IndexerError> {
        assert_eq!(header, self.cookies.clone().unwrap_or_default());
        Ok(())
    }

    fn content(&self) -> Result<String, IndexerError> {
        Ok(self.html.clone())
    }
}

fn write_render_overlay(dir: &std::path::Path) {
    fs::write(
        dir.join("demo.yaml"),
        r#"
id: demo
framework: nexusphp
render: true
search:
  path: /torrents.php
  query_param: search
list:
  item: "tr.torrent"
fields:
  title:
    selector: "a.name"
    attr: title
  enclosure:
    selector: "a.download"
    attr: href
  size:
    selector: "td.size"
  seeders:
    selector: "td.seeders"
  free:
    selector: "img.pro_free"
  hr:
    selector: "img.hitandrun"
rss:
  item: item
  title:
    selector: title
  enclosure:
    selector: enclosure
    attr: url
"#,
    )
    .unwrap();
}

#[test]
fn default_browser_config_is_disabled() {
    let off = BrowserConfig::disabled();
    assert!(!off.enabled);
    assert!(off.endpoint.is_none());
    assert!(off.managed_endpoint.is_none());
}

#[test]
fn recording_fetcher_never_launches_chromium_for_injected_html() {
    let overlay = tempfile::tempdir().unwrap();
    write_render_overlay(overlay.path());
    let html = include_str!("fixtures/nexusphp.html");
    let site = site_with_profile("demo", Some("ws://example"));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let fetcher = RecordingFetcher::new(
        HashMap::from([(format!("search:{}", site.id), Ok(html.to_string()))]),
        requests.clone(),
    );
    let indexer = Indexer::new(
        ProfileSet::load(Some(overlay.path())).unwrap(),
        Arc::new(fetcher),
    );
    let outcome = indexer.search(&[site], "matrix");
    assert_eq!(outcome.torrents.len(), 1);
    let captured = requests.lock().unwrap();
    assert_eq!(captured[0].cdp_url.as_deref(), Some("ws://example"));
}

#[test]
fn browser_with_external_cdp_propagates_injected_connection_failure() {
    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), |endpoint| {
        assert_eq!(endpoint, Some("http://fixture.invalid:9222"));
        Err(IndexerError::Fetch("fixture CDP connection failed".into()))
    });
    let req = FetchRequest {
        key: "test".into(),
        url: "https://fixture.invalid/document".into(),
        method: indexer::FetchMethod::Get,
        body: None,
        cookie: None,
        api_key: None,
        proxy: None,
        cdp_url: Some("http://fixture.invalid:9222".into()),
    };
    // Open failures happen before navigation, so they surface as BrowserSetup.
    let err = browser.fetch_html(&req).unwrap_err();
    assert!(matches!(err, IndexerError::BrowserSetup(_)), "{err:?}");
    assert!(err.to_string().contains("fixture CDP connection failed"));
}

#[test]
fn timing_constraints_are_validated() {
    use std::time::Duration;
    let b = Browser::new(BrowserConfig::disabled())
        .with_render_deadline(Duration::from_secs(10))
        .with_min_work(Duration::from_secs(10));
    assert!(b.validate_timing().is_err());

    let b2 = Browser::new(BrowserConfig::disabled())
        .with_render_deadline(Duration::from_secs(30))
        .with_min_work(Duration::from_secs(10))
        .with_queue_cap(Duration::from_secs(25));
    assert!(b2.validate_timing().is_err());

    let valid = Browser::new(BrowserConfig::disabled());
    assert!(valid.validate_timing().is_ok());
}

#[test]
fn render_returns_html_and_read_back_cookie() {
    struct TestCookiePage;
    impl PageSession for TestCookiePage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> { Ok(()) }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> { Ok(()) }
        fn content(&self) -> Result<String, IndexerError> { Ok("<html>ok</html>".into()) }
        fn cookie_header(&self, _: &str) -> Result<String, IndexerError> { Ok("session=123".into()) }
    }

    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), |_| {
        Ok(Arc::new(TestCookiePage) as Arc<dyn PageSession>)
    });
    let req = FetchRequest {
        key: "test".into(),
        url: "https://example.org/search".into(),
        method: FetchMethod::Get,
        body: None,
        cookie: Some("session=old".into()),
        api_key: None,
        proxy: None,
        cdp_url: Some("http://127.0.0.1:9222".into()),
    };
    let res = browser.render(&req).unwrap();
    assert_eq!(res.html, "<html>ok</html>");
    assert_eq!(res.cookie_header.as_deref(), Some("session=123"));
}

#[test]
fn target_is_closed_when_goto_fails() {
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let closed_clone = closed.clone();
    struct FailGotoPage(Arc<std::sync::atomic::AtomicBool>);
    impl PageSession for FailGotoPage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> {
            Err(IndexerError::Fetch("navigation rejected".into()))
        }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> { Ok(()) }
        fn content(&self) -> Result<String, IndexerError> { Ok("".into()) }
    }
    impl Drop for FailGotoPage {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
        Ok(Arc::new(FailGotoPage(closed_clone.clone())) as Arc<dyn PageSession>)
    });
    let req = FetchRequest {
        key: "test".into(),
        url: "https://example.org/search".into(),
        method: FetchMethod::Get,
        body: None,
        cookie: None,
        api_key: None,
        proxy: None,
        cdp_url: Some("http://127.0.0.1:9222".into()),
    };
    assert!(browser.render(&req).is_err());
    assert!(closed.load(std::sync::atomic::Ordering::Relaxed));
}

#[test]
fn websocket_url_on_a_different_host_or_port_is_rejected() {
    // Discovery endpoint is 127.0.0.1:9222, but returns ws://10.0.0.1:9222
    let fixture = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_clone = stop.clone();
        let handle = std::thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok((mut stream, _)) = listener.accept() {
                    let mut buf = [0; 1024];
                    let _ = stream.read(&mut buf);
                    let body = serde_json::json!({
                        "id": "target1",
                        "webSocketDebuggerUrl": "ws://10.0.0.9:9999/devtools/page/x"
                    }).to_string();
                    let resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    let _ = stream.write_all(resp.as_bytes());
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
        (format!("http://{addr}"), stop, handle)
    };

    let res = indexer::cdp_page::open_cdp_session(&fixture.0);
    fixture.1.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = fixture.2.join();
    let err = match res {
        Err(e) => e,
        Ok(_) => panic!("expected mismatched debugger url error"),
    };
    assert!(err.to_string().contains("mismatched debugger url"));
}

#[test]
fn different_sites_render_concurrently_up_to_the_limit() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let active = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(std::sync::Barrier::new(4));

    struct ConcurrentPage {
        active: Arc<AtomicUsize>,
        max_concurrent: Arc<AtomicUsize>,
        barrier: Arc<std::sync::Barrier>,
    }
    impl PageSession for ConcurrentPage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> {
            let cur = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = self.max_concurrent.fetch_max(cur, Ordering::SeqCst);
            self.barrier.wait();
            std::thread::sleep(Duration::from_millis(20));
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> { Ok(()) }
        fn content(&self) -> Result<String, IndexerError> { Ok("ok".into()) }
    }

    let active_c = active.clone();
    let max_c = max_concurrent.clone();
    let barrier_c = barrier.clone();
    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
        Ok(Arc::new(ConcurrentPage {
            active: active_c.clone(),
            max_concurrent: max_c.clone(),
            barrier: barrier_c.clone(),
        }) as Arc<dyn PageSession>)
    }).with_max_sessions(3);

    let mut handles = Vec::new();
    for site in ["https://site1.org/s", "https://site2.org/s", "https://site3.org/s"] {
        let b = browser.clone();
        let s = site.to_string();
        handles.push(std::thread::spawn(move || {
            let req = FetchRequest {
                key: "k".into(),
                url: s,
                method: FetchMethod::Get,
                body: None,
                cookie: None,
                api_key: None,
                proxy: None,
                        cdp_url: Some("http://127.0.0.1:9222".into()),
            };
            b.render(&req)
        }));
    }

    barrier.wait();
    for h in handles {
        assert!(h.join().unwrap().is_ok());
    }
    assert_eq!(max_concurrent.load(Ordering::SeqCst), 3);
}

#[test]
fn same_site_sessions_are_serialized() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let active = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));

    struct SerialPage {
        active: Arc<AtomicUsize>,
        max_concurrent: Arc<AtomicUsize>,
    }
    impl PageSession for SerialPage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> {
            let cur = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = self.max_concurrent.fetch_max(cur, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(40));
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> { Ok(()) }
        fn content(&self) -> Result<String, IndexerError> { Ok("ok".into()) }
    }

    let active_c = active.clone();
    let max_c = max_concurrent.clone();
    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
        Ok(Arc::new(SerialPage {
            active: active_c.clone(),
            max_concurrent: max_c.clone(),
        }) as Arc<dyn PageSession>)
    }).with_max_sessions(3);

    let mut handles = Vec::new();
    // Same eTLD+1: example.org
    for site in ["https://a.example.org/s", "https://b.example.org/s"] {
        let b = browser.clone();
        let s = site.to_string();
        handles.push(std::thread::spawn(move || {
            let req = FetchRequest {
                key: "k".into(),
                url: s,
                method: FetchMethod::Get,
                body: None,
                cookie: None,
                api_key: None,
                proxy: None,
                        cdp_url: Some("http://127.0.0.1:9222".into()),
            };
            b.render(&req)
        }));
    }

    for h in handles {
        assert!(h.join().unwrap().is_ok());
    }
    assert_eq!(max_concurrent.load(Ordering::SeqCst), 1);
}

#[test]
fn cousin_and_cross_level_subdomains_are_serialized() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let active = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));

    struct SerialPage(Arc<AtomicUsize>, Arc<AtomicUsize>);
    impl PageSession for SerialPage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> {
            let cur = self.0.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = self.1.fetch_max(cur, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(40));
            self.0.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> { Ok(()) }
        fn content(&self) -> Result<String, IndexerError> { Ok("ok".into()) }
    }

    let active_c = active.clone();
    let max_c = max_concurrent.clone();
    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
        Ok(Arc::new(SerialPage(active_c.clone(), max_c.clone())) as Arc<dyn PageSession>)
    }).with_max_sessions(3);

    let mut handles = Vec::new();
    for site in ["https://x.a.example.org/s", "https://y.b.example.org/s"] {
        let b = browser.clone();
        let s = site.to_string();
        handles.push(std::thread::spawn(move || {
            let req = FetchRequest {
                key: "k".into(),
                url: s,
                method: FetchMethod::Get,
                body: None,
                cookie: None,
                api_key: None,
                proxy: None,
                        cdp_url: Some("http://127.0.0.1:9222".into()),
            };
            b.render(&req)
        }));
    }

    for h in handles {
        assert!(h.join().unwrap().is_ok());
    }
    assert_eq!(max_concurrent.load(Ordering::SeqCst), 1);
}

#[test]
fn waiting_beyond_the_queue_cap_is_busy_before_navigation() {
    use std::time::Duration;

    struct BlockPage;
    impl PageSession for BlockPage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> {
            std::thread::sleep(Duration::from_millis(200));
            Ok(())
        }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> { Ok(()) }
        fn content(&self) -> Result<String, IndexerError> { Ok("ok".into()) }
    }

    let opened_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let opened_count_c = opened_count.clone();

    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
        opened_count_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Arc::new(BlockPage) as Arc<dyn PageSession>)
    })
    .with_max_sessions(1)
    .with_render_deadline(Duration::from_secs(2))
    .with_queue_cap(Duration::from_millis(50))
    .with_min_work(Duration::from_millis(10));

    let b1 = browser.clone();
    let h1 = std::thread::spawn(move || {
        let req = FetchRequest {
            key: "1".into(),
            url: "https://site1.org/s".into(),
            method: FetchMethod::Get,
            body: None,
            cookie: None,
            api_key: None,
            proxy: None,
                cdp_url: Some("http://127.0.0.1:9222".into()),
        };
        b1.render(&req)
    });

    std::thread::sleep(Duration::from_millis(20));

    let b2 = browser.clone();
    let res2 = b2.render(&FetchRequest {
        key: "2".into(),
        url: "https://site2.org/s".into(),
        method: FetchMethod::Get,
        body: None,
        cookie: None,
        api_key: None,
        proxy: None,
        cdp_url: Some("http://127.0.0.1:9222".into()),
    });

    assert!(h1.join().unwrap().is_ok());
    assert!(matches!(res2, Err(IndexerError::BrowserBusy)));
    assert_eq!(opened_count.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn too_little_time_left_after_queueing_is_busy_before_navigation() {
    use std::time::Duration;

    struct SleepPage;
    impl PageSession for SleepPage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> {
            std::thread::sleep(Duration::from_millis(1150));
            Ok(())
        }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> { Ok(()) }
        fn content(&self) -> Result<String, IndexerError> { Ok("ok".into()) }
    }

    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), |_| {
        Ok(Arc::new(SleepPage) as Arc<dyn PageSession>)
    })
    .with_max_sessions(1)
    .with_render_deadline(Duration::from_millis(1200))
    .with_queue_cap(Duration::from_millis(1100))
    .with_min_work(Duration::from_millis(100));

    let b1 = browser.clone();
    let h1 = std::thread::spawn(move || {
        b1.render(&FetchRequest {
            key: "1".into(),
            url: "https://site1.org/s".into(),
            method: FetchMethod::Get,
            body: None,
            cookie: None,
            api_key: None,
            proxy: None,
                cdp_url: Some("http://127.0.0.1:9222".into()),
        })
    });

    std::thread::sleep(Duration::from_millis(20));

    let b2 = browser.clone();
    let res2 = b2.render(&FetchRequest {
        key: "2".into(),
        url: "https://site2.org/s".into(),
        method: FetchMethod::Get,
        body: None,
        cookie: None,
        api_key: None,
        proxy: None,
        cdp_url: Some("http://127.0.0.1:9222".into()),
    });

    let h1_res = h1.join().unwrap();
    assert!(h1_res.is_ok());
    assert!(matches!(res2, Err(IndexerError::BrowserBusy)));
}

#[test]
fn waiters_are_served_in_fifo_order() {
    use std::time::Duration;

    let finish_order = Arc::new(Mutex::new(Vec::new()));
    let finish_order_c = finish_order.clone();

    struct SlowPage(usize, Arc<Mutex<Vec<usize>>>);
    impl PageSession for SlowPage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> {
            std::thread::sleep(Duration::from_millis(50));
            self.1.lock().unwrap().push(self.0);
            Ok(())
        }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> { Ok(()) }
        fn content(&self) -> Result<String, IndexerError> { Ok("ok".into()) }
    }

    let finish_log = finish_order.clone();
    let next_id = Arc::new(std::sync::atomic::AtomicUsize::new(1));
    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
        let id = next_id.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Arc::new(SlowPage(id, finish_log.clone())) as Arc<dyn PageSession>)
    })
    .with_max_sessions(1)
    .with_render_deadline(Duration::from_secs(3))
    .with_queue_cap(Duration::from_secs(2))
    .with_min_work(Duration::from_millis(10));

    let mut handles = Vec::new();
    for site in ["https://s1.org/s", "https://s2.org/s", "https://s3.org/s"] {
        let b = browser.clone();
        let s = site.to_string();
        handles.push(std::thread::spawn(move || {
            b.render(&FetchRequest {
                key: "k".into(),
                url: s,
                method: FetchMethod::Get,
                body: None,
                cookie: None,
                api_key: None,
                proxy: None,
                        cdp_url: Some("http://127.0.0.1:9222".into()),
            })
        }));
        std::thread::sleep(Duration::from_millis(15));
    }

    for h in handles {
        assert!(h.join().unwrap().is_ok());
    }
    assert_eq!(*finish_order_c.lock().unwrap(), vec![1, 2, 3]);
}

#[test]
fn unrelated_registrable_domains_can_render_concurrently() {
    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), |_| {
        struct QuickPage;
        impl PageSession for QuickPage {
            fn goto(&self, _: &str) -> Result<(), IndexerError> { Ok(()) }
            fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> { Ok(()) }
            fn content(&self) -> Result<String, IndexerError> { Ok("ok".into()) }
        }
        Ok(Arc::new(QuickPage) as Arc<dyn PageSession>)
    }).with_max_sessions(2);

    let res1 = browser.render(&FetchRequest {
        key: "1".into(),
        url: "https://example.org/s".into(),
        method: FetchMethod::Get,
        body: None,
        cookie: None,
        api_key: None,
        proxy: None,
        cdp_url: Some("http://127.0.0.1:9222".into()),
    });
    let res2 = browser.render(&FetchRequest {
        key: "2".into(),
        url: "https://other.org/s".into(),
        method: FetchMethod::Get,
        body: None,
        cookie: None,
        api_key: None,
        proxy: None,
        cdp_url: Some("http://127.0.0.1:9222".into()),
    });
    assert!(res1.is_ok());
    assert!(res2.is_ok());
}

#[test]
fn render_respects_the_total_deadline_and_closes_the_target() {
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let closed_c = closed.clone();

    struct TimeoutPage {
        closed: Arc<std::sync::atomic::AtomicBool>,
        deadline: Mutex<Option<Instant>>,
    }
    impl PageSession for TimeoutPage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> {
            std::thread::sleep(std::time::Duration::from_millis(650));
            if let Some(d) = *self.deadline.lock().unwrap() {
                if Instant::now() > d {
                    return Err(IndexerError::Fetch("deadline exceeded".into()));
                }
            }
            Ok(())
        }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> { Ok(()) }
        fn content(&self) -> Result<String, IndexerError> {
            std::thread::sleep(std::time::Duration::from_millis(650));
            if let Some(d) = *self.deadline.lock().unwrap() {
                if Instant::now() > d {
                    return Err(IndexerError::Fetch("deadline exceeded".into()));
                }
            }
            Ok("ok".into())
        }
        fn set_deadline(&self, deadline: Instant) {
            *self.deadline.lock().unwrap() = Some(deadline);
        }
    }
    impl Drop for TimeoutPage {
        fn drop(&mut self) {
            self.closed.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
        Ok(Arc::new(TimeoutPage {
            closed: closed_c.clone(),
            deadline: Mutex::new(None),
        }) as Arc<dyn PageSession>)
    })
    .with_render_deadline(std::time::Duration::from_millis(1200))
    .with_queue_cap(std::time::Duration::from_millis(1000))
    .with_min_work(std::time::Duration::from_millis(100));

    let start = std::time::Instant::now();
    let res = browser.render(&FetchRequest {
        key: "t".into(),
        url: "https://timeout.org/s".into(),
        method: FetchMethod::Get,
        body: None,
        cookie: None,
        api_key: None,
        proxy: None,
        cdp_url: Some("http://127.0.0.1:9222".into()),
    });
    assert!(res.is_err());
    assert!(start.elapsed() < std::time::Duration::from_millis(2500));
    assert!(closed.load(std::sync::atomic::Ordering::Relaxed));
}


fn plain_req(proxy: Option<&str>) -> FetchRequest {
    FetchRequest {
        key: "t".into(),
        url: "https://example.org/".into(),
        method: FetchMethod::Get,
        body: None,
        cookie: Some("a=b".into()),
        api_key: None,
        proxy: proxy.map(Into::into),
        cdp_url: Some("http://127.0.0.1:9222".into()),
    }
}

#[test]
fn render_rejects_proxy_before_opening_a_target() {
    let opened = Arc::new(Mutex::new(0));
    let o = opened.clone();
    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
        *o.lock().unwrap() += 1;
        Err(IndexerError::BrowserSetup("unused".into()))
    });
    let err = browser.render(&plain_req(Some("http://proxy:8080"))).unwrap_err();
    assert!(matches!(err, IndexerError::BrowserSetup(_)), "{err:?}");
    assert_eq!(*opened.lock().unwrap(), 0);
}

#[test]
fn cookie_injection_failure_is_setup_but_content_failure_is_not() {
    struct P(bool);
    impl PageSession for P {
        fn goto(&self, _: &str) -> Result<(), IndexerError> { Ok(()) }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> {
            if self.0 { Err(IndexerError::Fetch("setCookies".into())) } else { Ok(()) }
        }
        fn content(&self) -> Result<String, IndexerError> { Err(IndexerError::Fetch("after goto".into())) }
    }
    let b = Browser::with_opener(BrowserConfig::enabled_for_test(), |_| Ok(Arc::new(P(true)) as Arc<dyn PageSession>));
    assert!(matches!(b.render(&plain_req(None)), Err(IndexerError::BrowserSetup(_))));
    let b = Browser::with_opener(BrowserConfig::enabled_for_test(), |_| Ok(Arc::new(P(false)) as Arc<dyn PageSession>));
    assert!(matches!(b.render(&plain_req(None)), Err(IndexerError::Fetch(_))));
}

#[test]
fn try_group_key_errors_on_unnormalizable_host() {
    assert!(indexer::browser_host::try_group_key("").is_err());
    assert_eq!(indexer::browser_host::try_group_key("a.example.org").unwrap(), "example.org");
}
