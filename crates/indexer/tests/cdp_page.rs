use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use indexer::{Browser, BrowserConfig, FetchMethod, FetchRequest, IndexerError, PageSession, cdp_page::open_cdp_session};
use serde_json::{Value, json};
use tungstenite::Message;

struct Fixture {
    url: String,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<String>>>,
    commands: Arc<Mutex<Vec<Value>>>,
    thread: Option<JoinHandle<()>>,
}

impl Fixture {
    fn start(mode: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}");
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let commands = Arc::new(Mutex::new(Vec::new()));
        let thread_stop = stop.clone();
        let log = requests.clone();
        let cmd_log = commands.clone();
        let thread = thread::spawn(move || {
            let mut workers = Vec::new();
            while !thread_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let log = log.clone();
                        let cmd_log = cmd_log.clone();
                        workers.push(thread::spawn(move || serve(stream, address, mode, log, cmd_log)));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self {
            url,
            stop,
            requests,
            commands,
            thread: Some(thread),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn serve(
    mut stream: TcpStream,
    address: std::net::SocketAddr,
    mode: &str,
    log: Arc<Mutex<Vec<String>>>,
    cmd_log: Arc<Mutex<Vec<Value>>>,
) {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let mut peek = [0; 2048];
    let length = loop {
        let length = stream.peek(&mut peek).unwrap();
        if peek[..length].windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            break length;
        }
        thread::sleep(Duration::from_millis(1));
    };
    let request = String::from_utf8_lossy(&peek[..length]);
    if request.starts_with("GET /ws") || request.starts_with("GET /devtools") {
        if mode == "handshake" {
            let mut bytes = [0; 2048];
            while stream.read(&mut bytes).unwrap_or(0) > 0 {}
            return;
        }
        serve_ws(stream, mode, log, cmd_log);
        return;
    }
    let mut bytes = [0; 2048];
    let length = stream.read(&mut bytes).unwrap();
    let request = String::from_utf8_lossy(&bytes[..length]);
    log.lock()
        .unwrap()
        .push(request.lines().next().unwrap().into());
    let body = if request.contains("/json/version") {
        json!({"Browser":"Fixture/1", "webSocketDebuggerUrl":format!("ws://{address}/ws/browser")}).to_string()
    } else {
        "true".into()
    };
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}

fn serve_ws(
    stream: TcpStream,
    mode: &str,
    log: Arc<Mutex<Vec<String>>>,
    cmd_log: Arc<Mutex<Vec<Value>>>,
) {
    let mut socket = tungstenite::accept(stream).unwrap();
    let mut cookie = false;
    let mut network = false;
    let mut navigated = false;
    let mut polls = 0;
    while let Ok(message) = socket.read() {
        let Message::Text(text) = message else {
            continue;
        };
        let command: Value = serde_json::from_str(&text).unwrap();
        let method = command["method"].as_str().unwrap();
        log.lock().unwrap().push(method.into());
        cmd_log.lock().unwrap().push(command.clone());
        if mode == "silent" && method == "Runtime.evaluate" {
            continue;
        }
        if method == "Network.enable" {
            network = true;
        }
        if method == "Network.setExtraHTTPHeaders" {
            cookie = command["params"]["headers"]["Cookie"] == "session=secret";
        }
        if method == "Network.setCookies" {
            if let Some(cookies) = command["params"]["cookies"].as_array() {
                cookie = cookies.iter().any(|c| c["name"] == "session" && c["value"] == "secret")
                    || cookies.iter().any(|c| c["name"] == "uid" && c["value"] == "1");
            }
        }
        if method == "Page.navigate" {
            navigated = cookie && network;
        }
        let result = if method == "Runtime.evaluate" && mode == "success" && !navigated {
            json!({"id":command["id"], "result":{"result":{"type":"string","value":"<html>unauthenticated</html>"}}})
        } else {
            response(&command, mode, cookie && network, &mut polls)
        };
        for event in [
            json!({"method":"Page.lifecycleEvent","params":{"name":"load","frameId":"iframe","loaderId":"iframe-loader"}}),
            json!({"method":"Page.lifecycleEvent","params":{"name":"load","frameId":"frame","loaderId":if polls >= 3 {"loader"} else {"stale-loader"}}}),
            json!({"id":99999,"result":{}}),
            result,
        ] {
            if let Err(error) = socket.send(Message::Text(event.to_string().into())) {
                eprintln!("fixture send: {error}");
                return;
            }
        }
    }
}

fn response(command: &Value, mode: &str, authenticated: bool, polls: &mut usize) -> Value {
    let method = command["method"].as_str().unwrap();
    if mode == "cookies_error" && method == "Network.getCookies" {
        return json!({"id": command["id"], "error": {"code": -1, "message": "cookies error"}});
    }
    let result = match method {
        "Page.navigate" if mode == "no_page" => {
            return json!({"id": command["id"], "error": {"code": -32601, "message": "No page for session"}});
        }
        "Target.createBrowserContext" if mode == "context_timeout" => {
            return json!({"id": command["id"], "error": {"code": -32000, "message": "temporarily unavailable"}});
        }
        "Target.createBrowserContext" if mode == "no_context" => {
            return json!({"id": command["id"], "error": {"code": -32601, "message": "unknown method"}});
        }
        "Target.createBrowserContext" => json!({"browserContextId": "ctx"}),
        "Target.createTarget" => json!({"targetId": "fixture"}),
        "Target.attachToTarget" if command["params"]["flatten"] == true => json!({"sessionId": "sess-1"}),
        "Target.attachToTarget" => {
            return json!({"id": command["id"], "error": {"code": -32000, "message": "flatten required"}});
        }
        "Page.navigate" if !authenticated || mode == "navigation" => {
            json!({"errorText":"navigation rejected"})
        }
        "Page.navigate" => json!({"frameId":"frame", "loaderId":"loader"}),
        "Runtime.evaluate" if mode == "evaluation" => {
            json!({"exceptionDetails":{"text":"script failed"}})
        }
        "Runtime.evaluate" if command["params"]["expression"] == "document.readyState" => {
            *polls += 1;
            let state = if mode == "loading" || (mode != "success" && *polls < 3) {
                "loading"
            } else {
                "complete"
            };
            json!({"result":{"type":"string","value":state}})
        }
        "Runtime.evaluate" if mode == "missing_html" => json!({"result":{"type":"undefined"}}),
        "Runtime.evaluate" => {
            json!({"result":{"type":"string","value":"<html>authenticated</html>"}})
        }
        "Network.getCookies" if mode == "no_cookies" => {
            json!({"cookies": []})
        }
        "Network.getCookies" if mode == "dedup_cookies" => {
            json!({"cookies": [
                {"name": "sid", "value": "old", "path": "/"},
                {"name": "uid", "value": "1", "path": "/"},
                {"name": "sid", "value": "new", "path": "/forum"},
            ]})
        }
        "Network.getCookies" => {
            json!({"cookies": [
                {"name": "zebra", "value": "100"},
                {"name": "alpha", "value": "200"},
            ]})
        }
        _ => json!({}),
    };
    if mode == "protocol" && method == "Network.enable" {
        json!({"id":command["id"],"error":{"code":-1,"message":"disabled"}})
    } else {
        json!({"id":command["id"],"result":result})
    }
}

#[test]
fn cookie_navigation_and_content_share_authenticated_connection() {
    let fixture = Fixture::start("success");
    let page = open_cdp_session(&fixture.url).unwrap();
    page.set_site_cookies("https://site.invalid/search", "session=secret").unwrap();
    page.goto("https://site.invalid/search").unwrap();
    assert_eq!(page.content().unwrap(), "<html>authenticated</html>");
    drop(page);
    let requests = fixture.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|request| request == "Target.closeTarget")
    );
    assert!(
        requests
            .iter()
            .filter(|request| *request == "Runtime.evaluate")
            .count()
            >= 4
    );
}

#[test]
fn navigation_and_evaluation_failures_are_not_success() {
    for mode in ["navigation", "evaluation", "missing_html"] {
        let fixture = Fixture::start(mode);
        let page = open_cdp_session(&fixture.url).unwrap();
        page.set_site_cookies("https://site.invalid/search", "session=secret").unwrap();
        let result = if mode == "missing_html" {
            page.content().map(|_| ())
        } else {
            page.goto("https://site.invalid/search")
        };
        assert!(result.is_err(), "{mode} must fail");
    }
}

#[test]
fn initialization_failure_cleans_up_created_target() {
    let fixture = Fixture::start("protocol");
    assert!(open_cdp_session(&fixture.url).is_err());
    assert!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request == "Target.closeTarget")
    );
}

#[test]
fn document_readiness_has_a_finite_deadline() {
    let fixture = Fixture::start("loading");
    let page = open_cdp_session(&fixture.url).unwrap();
    page.set_site_cookies("https://site.invalid/search", "session=secret").unwrap();
    let start = Instant::now();
    assert!(page.goto("https://site.invalid/search").is_err());
    assert!(start.elapsed() >= Duration::from_secs(4));
    assert!(start.elapsed() < Duration::from_secs(7));
}

#[test]
fn handshake_timeout_is_bounded_setup_error_and_creates_nothing() {
    let fixture = Fixture::start("handshake");
    let start = Instant::now();
    let err = match open_cdp_session(&fixture.url) { Err(e) => e, Ok(_) => panic!("must fail") };
    assert!(matches!(err, IndexerError::BrowserSetup(_)), "{err:?}");
    assert!(start.elapsed() < Duration::from_secs(7));
    assert!(!fixture.requests.lock().unwrap().iter().any(|r| r == "Target.createTarget"));
}

#[test]
fn page_commands_use_the_flat_session_and_navigate_minus_32601_is_setup() {
    let fixture = Fixture::start("success");
    {
        let page = open_cdp_session(&fixture.url).unwrap();
        page.set_site_cookies("https://site.invalid/search", "session=secret").unwrap();
        page.goto("https://site.invalid/search").unwrap();
    }
    let cmds = fixture.commands.lock().unwrap().clone();
    for c in &cmds {
        let m = c["method"].as_str().unwrap();
        let page_level = m.starts_with("Page.") || m.starts_with("Network.") || m.starts_with("Runtime.");
        if page_level {
            assert_eq!(c["sessionId"], "sess-1", "{m} must carry the flat sessionId");
        } else {
            assert!(c.get("sessionId").is_none(), "{m} is browser-level");
        }
    }
    let fixture = Fixture::start("no_page");
    let page = open_cdp_session(&fixture.url).unwrap();
    let err = page.goto("https://site.invalid/").unwrap_err();
    assert!(matches!(err, IndexerError::BrowserSetup(_)), "{err:?}");
}

#[test]
fn transient_context_error_is_not_marked_incompatible() {
    let fixture = Fixture::start("context_timeout");
    let err = match open_cdp_session(&fixture.url) { Err(e) => e, Ok(_) => panic!("must fail") };
    assert!(!err.to_string().contains(indexer::cdp_page::INCOMPATIBLE_PREFIX), "{err}");
}

#[test]
fn missing_command_response_times_out_and_invalidates_connection() {
    let fixture = Fixture::start("silent");
    let page = open_cdp_session(&fixture.url).unwrap();
    let start = Instant::now();
    assert!(page.content().is_err());
    assert!(start.elapsed() < Duration::from_secs(7));
    let start = Instant::now();
    assert!(page.content().is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn site_cookies_are_bound_to_the_site_host_not_sent_as_extra_headers() {
    let fixture = Fixture::start("success");
    let page = open_cdp_session(&fixture.url).unwrap();
    page.set_site_cookies("http://127.0.0.1:8080/index.php", "uid=1; pass=abc").unwrap();

    let requests = fixture.requests.lock().unwrap();
    assert!(requests.iter().any(|r| r == "Network.setCookies"));
    assert!(!requests.iter().any(|r| r == "Network.setExtraHTTPHeaders"));

    let commands = fixture.commands.lock().unwrap();
    let set_cookie_cmd = commands.iter().find(|c| c["method"] == "Network.setCookies").unwrap();
    let cookies = set_cookie_cmd["params"]["cookies"].as_array().unwrap();
    assert_eq!(cookies.len(), 2);
    assert_eq!(cookies[0]["name"], "uid");
    assert_eq!(cookies[0]["value"], "1");
    assert_eq!(cookies[0]["url"], "http://127.0.0.1:8080/");
    assert_eq!(cookies[0]["secure"], false);
    assert_eq!(cookies[1]["name"], "pass");
    assert_eq!(cookies[1]["value"], "abc");
    assert_eq!(cookies[1]["url"], "http://127.0.0.1:8080/");
}

#[test]
fn https_site_cookies_are_marked_secure() {
    let fixture = Fixture::start("success");
    let page = open_cdp_session(&fixture.url).unwrap();
    page.set_site_cookies("https://tracker.example/index.php", "uid=1").unwrap();

    let commands = fixture.commands.lock().unwrap();
    let set_cookie_cmd = commands.iter().find(|c| c["method"] == "Network.setCookies").unwrap();
    let cookies = set_cookie_cmd["params"]["cookies"].as_array().unwrap();
    assert_eq!(cookies[0]["secure"], true);
    assert_eq!(cookies[0]["url"], "https://tracker.example/");
}

#[test]
fn cookie_readback_requests_cookies_for_the_current_page_url() {
    let fixture = Fixture::start("success");
    let page = open_cdp_session(&fixture.url).unwrap();
    let header = page.cookie_header("https://tracker.example/index.php").unwrap();
    assert_eq!(header, "alpha=200; zebra=100");

    let commands = fixture.commands.lock().unwrap();
    let get_cookie_cmd = commands.iter().find(|c| c["method"] == "Network.getCookies").unwrap();
    let urls = get_cookie_cmd["params"]["urls"].as_array().unwrap();
    assert_eq!(urls, &vec![json!("https://tracker.example/index.php")]);
}

#[test]
fn cookie_readback_deduplicates_same_name_across_paths() {
    let fixture = Fixture::start("dedup_cookies");
    let page = open_cdp_session(&fixture.url).unwrap();
    let header = page.cookie_header("https://tracker.example/index.php").unwrap();
    // sid was old (path /) then new (path /forum). BTreeMap dedup preserves the latest value.
    // Only site-wide (path=/) cookies are read back; /forum is ignored.
    assert_eq!(header, "sid=old; uid=1");
}

#[test]
fn readback_failure_yields_none_and_keeps_the_html() {
    let fixture = Fixture::start("cookies_error");
    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), {
        let fixture_url = fixture.url.clone();
        move |_cdp| {
            open_cdp_session(&fixture_url).map(|s| Arc::new(s) as Arc<dyn PageSession>)
        }
    });
    let req = FetchRequest {
        key: "test".into(),
        url: format!("{}/search", fixture.url),
        method: FetchMethod::Get,
        body: None,
        cookie: Some("session=secret".into()),
        api_key: None,
        proxy: None,
        cdp_url: Some(fixture.url.clone()),
    };
    let rendered = browser.render(&req).unwrap();
    assert_eq!(rendered.html, "<html>authenticated</html>");
    assert_eq!(rendered.cookie_header, None);
}

#[test]
fn default_page_session_cookie_header_is_an_error_not_a_fake_session() {
    struct MinimalPage;
    impl PageSession for MinimalPage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> { Ok(()) }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> { Ok(()) }
        fn content(&self) -> Result<String, IndexerError> { Ok("".into()) }
    }

    let page = MinimalPage;
    assert!(page.cookie_header("http://example.com").is_err());
    let src = include_str!("../src/browser.rs");
    assert!(!src.contains("session=browser"));
}

#[test]
fn browser_session_never_deletes_or_clears_cookies() {
    let fixture = Fixture::start("success");
    let page = open_cdp_session(&fixture.url).unwrap();
    page.set_site_cookies("https://site.invalid/search", "session=secret").unwrap();
    page.goto("https://site.invalid/search").unwrap();
    let _ = page.content().unwrap();
    let _ = page.cookie_header("https://site.invalid/search").unwrap();
    drop(page);

    let requests = fixture.requests.lock().unwrap();
    assert!(!requests.iter().any(|r| r == "Network.deleteCookies"));
    assert!(!requests.iter().any(|r| r == "Network.clearBrowserCookies"));
    assert!(!requests.iter().any(|r| r == "Storage.clearCookies"));
}

#[test]
fn each_session_runs_in_its_own_browser_context_and_disposes_it() {
    let fixture = Fixture::start("success");
    {
        let page = open_cdp_session(&fixture.url).unwrap();
        page.set_site_cookies("https://site.invalid/", "session=secret").unwrap();
    }
    let cmds = fixture.commands.lock().unwrap().clone();
    let create = cmds.iter().find(|c| c["method"] == "Target.createTarget").expect("createTarget");
    assert_eq!(create["params"]["browserContextId"], "ctx");
    assert!(cmds.iter().any(|c| c["method"] == "Target.disposeBrowserContext"
        && c["params"]["browserContextId"] == "ctx"));
}

#[test]
fn endpoint_without_browser_contexts_is_incompatible_setup_error() {
    let fixture = Fixture::start("no_context");
    let err = match open_cdp_session(&fixture.url) { Err(e) => e, Ok(_) => panic!("must fail") };
    match err {
        IndexerError::BrowserSetup(m) => assert!(m.starts_with(indexer::cdp_page::INCOMPATIBLE_PREFIX), "{m}"),
        other => panic!("expected BrowserSetup, got {other:?}"),
    }
}
