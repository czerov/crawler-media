//! T0 compatibility spike against a real CDP endpoint (Obscura v0.2.3).
//! Ignored by default: run with
//! `OBSCURA_CDP_URL=http://127.0.0.1:<port> cargo test -p indexer --test obscura_t0 -- --ignored`
//! and record the results in ADR-0007's T0 verification table.

use indexer::PageSession;
use indexer::cdp_page::open_cdp_session;

fn endpoint() -> String {
    std::env::var("OBSCURA_CDP_URL").expect("set OBSCURA_CDP_URL to a live CDP endpoint")
}

#[test]
#[ignore = "needs a live Obscura endpoint (T0)"]
fn sessions_are_isolated_by_browser_context() {
    let url = "http://example.com/";
    {
        let a = open_cdp_session(&endpoint()).expect("createBrowserContext supported");
        a.set_site_cookies(url, "t0_probe=1").unwrap();
        assert!(a.cookie_header(url).unwrap().contains("t0_probe=1"), "getCookies{{urls}} reads back");
    }
    let b = open_cdp_session(&endpoint()).unwrap();
    assert!(
        !b.cookie_header(url).unwrap().contains("t0_probe"),
        "a new session must not see the previous session's cookies"
    );
}

fn local_site() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            log.lock().unwrap().push(String::from_utf8_lossy(&buf[..n]).into_owned());
            let body = "<html><body>t0-ok</body></html>";
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nSet-Cookie: srv=7; Path=/\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (format!("http://{addr}/index.php"), seen)
}

#[test]
#[ignore = "needs a live Obscura endpoint started with --allow-private-network (T0)"]
fn render_end_to_end_navigates_injects_and_reads_back_cookies() {
    use indexer::{Browser, BrowserConfig, FetchMethod, FetchRequest};
    let (url, seen) = local_site();
    let browser = Browser::new(BrowserConfig::enabled_for_test());
    let page = browser
        .render(&FetchRequest {
            key: "t0".into(),
            url: url.clone(),
            method: FetchMethod::Get,
            body: None,
            cookie: Some("uid=42".into()),
            api_key: None,
            proxy: None,
            cdp_url: Some(endpoint()),
        })
        .expect("render against real Obscura");
    assert!(page.html.contains("t0-ok"), "{}", page.html);
    let requests = seen.lock().unwrap().join("\n");
    assert!(requests.contains("uid=42"), "injected cookie must reach the site: {requests}");
    let header = page.cookie_header.unwrap_or_default();
    assert!(header.contains("srv=7"), "server cookie read back: {header}");
}
