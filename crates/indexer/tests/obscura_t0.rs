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
