use std::sync::{Arc, Mutex};

use domain::{Site, SiteId};
use hooks::{Bus, CheckInPlugin, HttpPost, LoginPlugin, PluginError, SiteCredentials};
use indexer::{Browser, BrowserConfig, IndexerError, PageSession};

struct Credentials(Mutex<Site>);
impl SiteCredentials for Credentials {
    fn load(&self, _: SiteId) -> Result<Site, PluginError> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, site: &Site) -> Result<(), PluginError> {
        *self.0.lock().unwrap() = site.clone();
        Ok(())
    }
}

fn credentials(cookie: Option<&str>) -> Credentials {
    Credentials(Mutex::new(Site {
        id: SiteId::new(),
        name: "fixture".into(),
        url: "https://pt.invalid".into(),
        profile_id: "demo".into(),
        cookie: cookie.map(str::to_string),
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: Some("http://fixture.invalid".into()),
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
    }))
}

struct Reply(&'static str);
impl HttpPost for Reply {
    fn get(&self, _: &str, _: Option<&str>, _: Option<&str>) -> Result<String, PluginError> {
        Ok(self.0.into())
    }
    fn post(
        &self,
        _: &str,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<String, PluginError> {
        Ok(self.0.into())
    }
}

const BAD_PAGES: &[&str] = &[
    "<html><form action='takelogin.php'><input name='password'/></form></html>",
    "<html><form><input name='captcha'/></form></html>",
    "<html>unknown response</html>",
    "Set-Cookie: uid=unverified\n<html>unknown response</html>",
    "<html><form action='takelogin.php'><input name='password'/></form>签到成功</html>",
    "<html><script>var message = '签到成功';</script></html>",
    "<html>如果签到成功，会看到奖励。</html>",
    "<html>未签到成功</html>",
    "<html><input name='CAPTCHA'>签到成功</html>",
    "<html><div>签到失败</div><div>签到成功</div></html>",
];

#[test]
fn fetched_login_form_challenge_and_unknown_page_never_authenticate() {
    for &body in BAD_PAGES {
        let store = credentials(Some("uid=valid; pass=secret"));
        let before = store.0.lock().unwrap().clone();
        assert!(
            LoginPlugin::http(&Bus::new(), &store, &Reply(body))
                .login(before.id)
                .is_err(),
            "{body}"
        );
        assert_eq!(store.0.lock().unwrap().cookie, before.cookie);
    }
}

#[test]
fn english_completed_and_already_completed_accept_normalized_whitespace() {
    for body in [
        "<div>Attendance successful</div>",
        "<div>Check-in \n successful!</div>",
        "<div>You have\t already attended.</div>",
        "<div>Attendance <strong>successful</strong>!</div>",
    ] {
        let store = credentials(Some("uid=valid"));
        let id = store.0.lock().unwrap().id;
        let result = CheckInPlugin::http(&Bus::new(), &store, &Reply(body)).check_in(id);
        assert!(result.is_ok(), "{body}: {result:?}");
    }
}

#[test]
fn split_negative_conditional_and_help_never_confirm_or_save_cookies() {
    for body in [
        "Set-Cookie: uid=unverified\n<div>未<span>签到成功</span></div>",
        "<div>如果<span>签到成功</span>，会看到奖励。</div>",
        "<div>如果<div>签到成功</div>会看到奖励。</div>",
        "<div>签到成功后会显示奖励</div>",
        "<div>签到成功！如果未获得奖励，请重试。</div>",
        "<div>签到成功！表示您可以领取奖励。</div>",
        "<div>签到成功！<span>这是帮助示例。</span></div>",
        "<aside class='help'><div>签到成功</div></aside>",
        "<div>帮助：<div>签到成功</div></div>",
        "<div>If <span>Attendance successful</span>, a reward will show.</div>",
        "<div>If <div>Attendance successful</div>, refresh this page.</div>",
        "<div class='instructions'><p>You have already attended</p></div>",
        "<div hidden>签到成功</div>",
        "<div aria-hidden='true'><span>签到成功</span></div>",
        "<div style='display: none'>签到成功</div>",
        "<div style='visibility: hidden'>签到成功</div>",
        "<style>签到成功</style><noscript>签到成功</noscript>",
        "Set-Cookie: uid=unverified\n<div>签到<span>失败</span></div><div>签到成功</div>",
        "<div>签到<div>失败</div></div><div>签到成功</div>",
        "<span class='help'>签到成功</span>",
        "<div>如果<span>签到成功</span>，获得 10 魔力</div>",
        "<div>签到成功！<span>获得 10 魔力</span>，请阅读说明。</div>",
        "<div>Check-in <span>failed</span></div><div>签到成功</div>",
        "<div>签到成功！获得奖励请查看帮助。</div>",
    ] {
        let store = credentials(Some("uid=valid"));
        let before = store.0.lock().unwrap().clone();
        let result = CheckInPlugin::http(&Bus::new(), &store, &Reply(body)).check_in(before.id);
        assert!(result.is_err(), "{body}");
        assert_eq!(store.0.lock().unwrap().cookie, before.cookie, "{body}");
    }
}

#[test]
fn split_reward_confirmation_still_passes() {
    for body in [
        "<div>签到<span>成功</span>！获得 <b>10</b> 魔力</div>",
        "<div>您今天已经<span>签到过了</span>！</div>",
        "<div hidden>帮助：签到成功</div><div>签到成功！获得 10 魔力</div>",
        "<main><div>签到成功！获得 10 魔力</div></main><footer><a>帮助</a></footer>",
        "<main><div>Attendance successful</div></main><footer><a>Help</a></footer>",
        "<main><div>签到成功</div><span>帮助</span></main>",
        "<main><div>签到成功</div><a href='/help'>帮助</a></main>",
    ] {
        let store = credentials(Some("uid=valid"));
        let id = store.0.lock().unwrap().id;
        let result = CheckInPlugin::http(&Bus::new(), &store, &Reply(body)).check_in(id);
        assert!(result.is_ok(), "{body}: {result:?}");
    }
}

#[test]
fn visible_success_with_reward_text_is_confirmed() {
    let store = credentials(Some("uid=valid"));
    let id = store.0.lock().unwrap().id;
    let reply = Reply("<html><div>签到成功！获得 10 魔力</div></html>");
    assert!(
        CheckInPlugin::http(&Bus::new(), &store, &reply)
            .check_in(id)
            .is_ok()
    );
}

#[test]
fn check_in_requires_verified_success_not_transport_success_or_cookie_rotation() {
    for &body in BAD_PAGES {
        let store = credentials(Some("uid=valid; pass=secret"));
        let before = store.0.lock().unwrap().clone();
        assert!(
            CheckInPlugin::http(&Bus::new(), &store, &Reply(body))
                .check_in(before.id)
                .is_err(),
            "{body}"
        );
        assert_eq!(store.0.lock().unwrap().cookie, before.cookie);
    }
}

#[test]
fn conflicting_check_in_confirmation_is_not_success() {
    let store = credentials(Some("uid=valid"));
    let id = store.0.lock().unwrap().id;
    let reply = Reply("<html><div>签到失败</div><div>签到成功</div></html>");
    assert!(
        CheckInPlugin::http(&Bus::new(), &store, &reply)
            .check_in(id)
            .is_err()
    );
}

#[test]
fn missing_login_credentials_are_actionable_not_empty_submission() {
    let store = credentials(None);
    let id = store.0.lock().unwrap().id;
    let result =
        LoginPlugin::http(&Bus::new(), &store, &Reply("Set-Cookie: uid=fabricated")).login(id);
    assert!(result.is_err());
    assert!(store.0.lock().unwrap().cookie.is_none());
}

struct Page(&'static str);
impl PageSession for Page {
    fn goto(&self, _: &str) -> Result<(), IndexerError> {
        Ok(())
    }
    fn set_cookie_header(&self, _: &str) -> Result<(), IndexerError> {
        Ok(())
    }
    fn content(&self) -> Result<String, IndexerError> {
        Ok(self.0.into())
    }
}

#[test]
fn browser_login_never_fabricates_cookie_from_an_unverified_page() {
    for &body in BAD_PAGES {
        let store = credentials(Some("uid=valid; pass=secret"));
        let before = store.0.lock().unwrap().clone();
        let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
            Ok(Arc::new(Page(body)) as Arc<dyn PageSession>)
        });
        assert!(
            LoginPlugin::browser(&Bus::new(), &store, &browser)
                .login(before.id)
                .is_err()
        );
        assert_eq!(store.0.lock().unwrap().cookie, before.cookie);
    }
}

#[test]
fn hook_dispatch_without_a_transport_is_not_a_completed_check_in() {
    let store = credentials(Some("uid=valid"));
    let id = store.0.lock().unwrap().id;
    assert!(
        CheckInPlugin::new(&Bus::new(), &store)
            .check_in(id)
            .is_err()
    );
}

#[test]
fn profile_specific_authentication_evidence_can_be_injected() {
    let store = credentials(Some("uid=valid"));
    let id = store.0.lock().unwrap().id;
    let policy = hooks::NexusPhpPolicy::new(Some("#profile-auth".into()));
    let bus = Bus::new();
    let reply = Reply("<html><div id='profile-auth'>Authenticated</div></html>");
    assert!(
        LoginPlugin::http(&bus, &store, &reply)
            .with_policy(&policy)
            .login(id)
            .is_ok()
    );
    assert!(LoginPlugin::http(&bus, &store, &reply).login(id).is_err());
}

struct AttendancePolicy;
impl hooks::SiteResponsePolicy for AttendancePolicy {
    fn authenticated(&self, _: &Site, _: &str) -> Result<(), PluginError> {
        Err(PluginError::Fetch("not an authentication contract".into()))
    }
    fn check_in(&self, _: &Site, body: &str) -> Result<hooks::CheckInOutcome, PluginError> {
        match body {
            "profile-complete" => Ok(hooks::CheckInOutcome::Completed),
            "profile-already-complete" => Ok(hooks::CheckInOutcome::AlreadyCompleted),
            _ => Err(PluginError::Fetch("unverified profile outcome".into())),
        }
    }
}

#[test]
fn profile_specific_completed_and_already_completed_contracts_can_be_injected() {
    let store = credentials(Some("uid=valid"));
    let id = store.0.lock().unwrap().id;
    let bus = Bus::new();
    for body in ["profile-complete", "profile-already-complete"] {
        let reply = Reply(body);
        assert!(
            CheckInPlugin::http(&bus, &store, &reply)
                .with_policy(&AttendancePolicy)
                .check_in(id)
                .is_ok()
        );
    }
    assert!(
        CheckInPlugin::http(&bus, &store, &Reply("unknown"))
            .with_policy(&AttendancePolicy)
            .check_in(id)
            .is_err()
    );
}

#[test]
fn confirmed_success_and_already_checked_in_are_both_valid_outcomes() {
    for body in ["<html>签到成功</html>", "<html>您今天已经签到过了</html>"] {
        let store = credentials(Some("uid=valid"));
        let id = store.0.lock().unwrap().id;
        assert!(
            CheckInPlugin::http(&Bus::new(), &store, &Reply(body))
                .check_in(id)
                .is_ok()
        );
    }
}
