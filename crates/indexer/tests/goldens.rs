use std::collections::HashMap;
use std::fs;
use std::sync::Arc;

use domain::{Site, SiteId};
use indexer::{FetchMethod, FetchRequest, Fetcher, Indexer, IndexerError, ProfileSet};

fn demo_site(id: SiteId, profile_id: &str) -> Site {
    Site {
        id,
        name: profile_id.into(),
        url: "https://pt.example/".into(),
        profile_id: profile_id.into(),
        cookie: Some("uid=1".into()),
        api_key: None,
        rss_url: Some("https://pt.example/rss".into()),
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
    }
}

struct Injected {
    bodies: HashMap<String, Result<String, String>>,
}

impl Fetcher for Injected {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        match self.bodies.get(&request.key) {
            Some(Ok(body)) => Ok(body.clone()),
            Some(Err(msg)) => Err(IndexerError::Fetch(msg.clone())),
            None => Err(IndexerError::Fetch(format!("missing {}", request.key))),
        }
    }
}

fn indexer_with(
    bodies: HashMap<String, Result<String, String>>,
    overlay: Option<&std::path::Path>,
) -> Indexer {
    let profiles = ProfileSet::load(overlay).unwrap();
    Indexer::new(profiles, Arc::new(Injected { bodies }))
}

#[test]
fn nexusphp_html_fixture_produces_torrent_fields() {
    let site = demo_site(SiteId::new(), "demo");
    let html = include_str!("fixtures/nexusphp.html");
    let indexer = indexer_with(
        HashMap::from([(format!("search:{}", site.id), Ok(html.to_string()))]),
        None,
    );

    let outcome = indexer.search(&[site.clone()], "matrix");
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(outcome.torrents.len(), 1);
    let torrent = &outcome.torrents[0];
    assert_eq!(torrent.site_id, site.id);
    assert_eq!(torrent.title, "The.Matrix.1999.2160p.BluRay.x265-GROUP");
    assert_eq!(
        torrent.enclosure,
        "https://pt.example/download.php?id=1&passkey=abc"
    );
    assert_eq!(torrent.size_bytes, Some(4 * 1024 * 1024 * 1024));
    assert_eq!(torrent.seeders, Some(42));
    assert!(torrent.free);
    assert!(torrent.hr);
    // U2 扩展字段。
    assert_eq!(torrent.id.as_deref(), Some("1"), "id 从下载链接提取");
    assert_eq!(torrent.leechers, Some(7));
    assert_eq!(torrent.snatched, Some(180));
    assert_eq!(torrent.upload_time.as_deref(), Some("2024-01-15 08:30"));
    assert_eq!(torrent.category.as_deref(), Some("Movies"));
    assert_eq!(
        torrent.detail_url.as_deref(),
        Some("https://pt.example/details.php?id=1")
    );
}

#[test]
fn mteam_json_fixture_uses_api_contract_and_produces_torrent_fields() {
    struct MTeamInjected;

    impl Fetcher for MTeamInjected {
        fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
            match request.key.as_str() {
                key if key.starts_with("search:") => {
                    assert_eq!(request.method, FetchMethod::PostJson);
                    assert_eq!(request.url, "https://api.m-team.cc/api/torrent/search");
                    assert_eq!(request.api_key.as_deref(), Some("secret-key"));
                    assert_eq!(
                        serde_json::from_str::<serde_json::Value>(
                            request.body.as_deref().expect("M-Team search body"),
                        )
                        .unwrap(),
                        serde_json::json!({
                            "mode": "normal",
                            "keyword": "dune",
                            "pageNumber": 1,
                            "pageSize": 100,
                        }),
                    );
                    Ok(include_str!("fixtures/mteam.json").into())
                }
                key if key.starts_with("download:") => {
                    assert_eq!(request.method, FetchMethod::PostForm);
                    assert_eq!(request.url, "https://api.m-team.cc/api/torrent/genDlToken");
                    assert_eq!(request.body.as_deref(), Some("id=99"));
                    Ok(include_str!("fixtures/mteam_download.json").into())
                }
                key => Err(IndexerError::Fetch(format!("unexpected {key}"))),
            }
        }
    }

    let mut site = demo_site(SiteId::new(), "mteam");
    // 即使用户填了网页端的 URL，也会通过 profile 的 base_url 自动映射到 api.m-team.cc
    site.url = "https://kp.m-team.cc/".into();
    site.api_key = Some("secret-key".into());
    let indexer = Indexer::new(ProfileSet::load(None).unwrap(), Arc::new(MTeamInjected));

    let outcome = indexer.search(&[site.clone()], "dune");
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    let torrent = &outcome.torrents[0];
    assert_eq!(torrent.title, "Dune.Part.Two.2024.1080p.BluRay.x264");
    assert_eq!(
        torrent.enclosure,
        "https://kp.m-team.cc/api/torrent/download?id=99"
    );
    assert_eq!(torrent.size_bytes, Some(21474836480));
    assert_eq!(torrent.seeders, Some(7));
    assert!(torrent.free);
    assert!(torrent.hr);
    assert_eq!(torrent.upload_time.as_deref(), Some("2024-03-01 12:00:00"));
    assert_eq!(
        torrent.poster_url.as_deref(),
        Some("https://img.m-team.cc/poster99.jpg")
    );
    assert_eq!(
        indexer.resolve_torrent_download(&site, torrent).unwrap(),
        Some("https://api.m-team.cc/api/rss/dlv2?sign=signed-token".into()),
    );
}

#[test]
fn mteam_category_scope_reaches_the_outbound_search_request() {
    struct CategoryFetcher;
    impl Fetcher for CategoryFetcher {
        fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
            let body: serde_json::Value =
                serde_json::from_str(request.body.as_deref().unwrap()).unwrap();
            assert_eq!(
                body["categories"],
                serde_json::json!([401, 419, 420, 421, 439])
            );
            Ok(include_str!("fixtures/mteam.json").into())
        }
    }
    let mut site = demo_site(SiteId::new(), "mteam");
    site.api_key = Some("test-key".into());
    let indexer = Indexer::new(ProfileSet::load(None).unwrap(), Arc::new(CategoryFetcher));
    let outcome = indexer.search_page_categories(&[site], "dune", 1, &["movie".into()]);
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(outcome.torrents.len(), 1);
}

#[test]
fn rss_xml_fixture_produces_torrent_fields() {
    let site = demo_site(SiteId::new(), "demo");
    let xml = include_str!("fixtures/rss.xml");
    let indexer = indexer_with(
        HashMap::from([(format!("rss:{}", site.id), Ok(xml.to_string()))]),
        None,
    );

    let outcome = indexer.rss(&[site.clone()]);
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    let torrent = &outcome.torrents[0];
    assert_eq!(torrent.title, "Arrival.2016.1080p.BluRay.x264-GROUP");
    assert_eq!(torrent.enclosure, "https://pt.example/download.php?id=8");
    assert_eq!(torrent.size_bytes, Some(8589934592));
    assert_eq!(torrent.seeders, Some(11));
    assert!(torrent.free);
    assert!(torrent.hr);
    // U2 扩展字段。
    assert_eq!(torrent.id.as_deref(), Some("8"), "RSS guid 兜底提取 id");
    assert_eq!(torrent.leechers, Some(3));
    assert_eq!(torrent.snatched, Some(95));
    assert_eq!(torrent.upload_time.as_deref(), Some("2024-02-01 12:00"));
    assert_eq!(torrent.category.as_deref(), Some("Movies"));
}

#[test]
fn one_site_failure_does_not_fail_the_other() {
    let ok = demo_site(SiteId::new(), "demo");
    let bad = demo_site(SiteId::new(), "demo");
    let html = include_str!("fixtures/nexusphp.html");
    let indexer = indexer_with(
        HashMap::from([
            (format!("search:{}", ok.id), Ok(html.to_string())),
            (format!("search:{}", bad.id), Err("timeout".into())),
        ]),
        None,
    );

    let outcome = indexer.search(&[ok.clone(), bad.clone()], "matrix");
    assert_eq!(outcome.torrents.len(), 1);
    assert_eq!(outcome.torrents[0].site_id, ok.id);
    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(outcome.failures[0].site_id, bad.id);
}

#[test]
fn user_overlay_replaces_builtin_profile_with_same_id() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("demo.yaml"),
        r#"
id: demo
framework: nexusphp
search:
  path: /torrents.php
  query_param: search
list:
  item: "tr.torrent"
fields:
  title:
    selector: "a.name"
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
  size:
    selector: enclosure
    attr: length
  seeders:
    selector: seeders
"#,
    )
    .unwrap();

    let site = demo_site(SiteId::new(), "demo");
    let html = include_str!("fixtures/nexusphp.html");
    let indexer = indexer_with(
        HashMap::from([(format!("search:{}", site.id), Ok(html.to_string()))]),
        Some(dir.path()),
    );

    let outcome = indexer.search(&[site], "matrix");
    assert_eq!(outcome.torrents[0].title, "Wrong Title");
}

#[test]
fn search_request_is_http_and_honors_render_flag_without_launching_chromium() {
    let site = demo_site(SiteId::new(), "demo");
    let html = include_str!("fixtures/nexusphp.html");
    let indexer = indexer_with(
        HashMap::from([(format!("search:{}", site.id), Ok(html.to_string()))]),
        None,
    );
    let outcome = indexer.search(&[site], "matrix");
    assert_eq!(outcome.torrents.len(), 1);
    assert!(!indexer.profile("demo").unwrap().render);
}

#[test]
fn pterclub_html_fixture_skips_header_and_parses_download_row() {
    let site = demo_site(SiteId::new(), "pterclub");
    let html = include_str!("fixtures/pterclub.html");
    let indexer = indexer_with(
        HashMap::from([(format!("search:{}", site.id), Ok(html.to_string()))]),
        None,
    );

    let outcome = indexer.search(&[site.clone()], "friends");
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(outcome.torrents.len(), 1);
    let torrent = &outcome.torrents[0];
    assert_eq!(torrent.title, "Friends S01E01 1080p WEB-DL H.264-GROUP");
    assert_eq!(
        torrent.enclosure,
        "https://pt.example/download.php?id=42&passkey=abc"
    );
    assert_eq!(torrent.size_bytes, Some(210 * 1024 * 1024));
    assert_eq!(torrent.seeders, Some(7));
    assert!(torrent.free);
    assert!(torrent.hr);
}

#[test]
fn pterclub_live_html_nested_torrentname_row_parses() {
    let site = demo_site(SiteId::new(), "pterclub");
    let html = include_str!("fixtures/pterclub-live.html");
    let indexer = indexer_with(
        HashMap::from([(format!("search:{}", site.id), Ok(html.to_string()))]),
        None,
    );

    let outcome = indexer.search(&[site.clone()], "friends");
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(outcome.torrents.len(), 1);
    let torrent = &outcome.torrents[0];
    assert_eq!(
        torrent.title,
        "Friends S09 2002 2160p NF WEB-DL H.265 DDP5.1-HHWEB"
    );
    assert_eq!(
        torrent.enclosure,
        "https://pt.example/download.php?id=42&passkey=abc"
    );
    assert_eq!(
        torrent.size_bytes,
        Some((87.52_f64 * 1024.0 * 1024.0 * 1024.0).round() as u64)
    );
    assert_eq!(torrent.seeders, Some(4));
    assert!(torrent.free);
    assert!(torrent.hr);
}

#[test]
fn expired_cookie_redirecting_to_login_triggers_auth_failure() {
    let site = demo_site(SiteId::new(), "pterclub");
    let html = r#"<!doctype html>
<html>
<head><title>ＰＴ之友俱乐部 :: 登录 PTerClub</title></head>
<body>
<form method="post" action="takelogin.php">
  <input type="text" name="username" />
  <input type="password" name="password" />
</form>
</body>
</html>"#;
    let indexer = indexer_with(
        HashMap::from([(format!("search:{}", site.id), Ok(html.to_string()))]),
        None,
    );

    let outcome = indexer.search(&[site.clone()], "friends");
    assert_eq!(outcome.torrents.len(), 0);
    assert_eq!(outcome.failures.len(), 1);
    assert!(
        outcome.failures[0].error.contains("Cookie 已过期"),
        "error={}",
        outcome.failures[0].error
    );
}

#[test]
fn all_builtin_profiles_load_and_have_core_fields() {
    let profiles = ProfileSet::load(None).unwrap();
    // 内置应有：demo + mteam + pterclub + U7 移植的 6 站。
    for id in [
        "demo",
        "mteam",
        "pterclub",
        "chdbits",
        "hdsky",
        "hddolby",
        "hdfans",
        "ourbits",
        "keepfrds",
        "agsvpt",
        "audiences",
        "hdarea",
        "hdhome",
        "hdtime",
        "hdvideo",
        "hhanclub",
        "nicept",
        "piggo",
        "pthome",
        "pttime",
        "soulvoice",
        "ssd",
        "tjupt",
        "ttg",
    ] {
        let profile = profiles
            .get(id)
            .unwrap_or_else(|| panic!("profile {id} 应内置"));
        assert!(
            profile.search.path.starts_with('/'),
            "profile {id} 的 search.path 应为绝对路径"
        );
        // API 框架（mteam）用 JSON 解析，无 HTML selector；NexusPHP 必须有。
        if id != "mteam" {
            assert!(
                profile.fields.title.is_some() && profile.fields.enclosure.is_some(),
                "profile {id} 必须有 title + enclosure 字段"
            );
        }
    }
    // U7 移植站应带 U2 扩展字段（leechers/snatched/upload_time/detail）。
    let chdbits = profiles.get("chdbits").unwrap();
    assert!(chdbits.fields.leechers.is_some());
    assert!(chdbits.fields.snatched.is_some());
    assert!(chdbits.fields.upload_time.is_some());
    assert!(chdbits.fields.detail.is_some());
    // 每站都有 RSS 映射（用户配 rss_url 后即可用）。
    for id in [
        "chdbits",
        "hdsky",
        "hddolby",
        "hdfans",
        "ourbits",
        "keepfrds",
        "agsvpt",
        "audiences",
        "hdarea",
        "hdhome",
        "hdtime",
        "hdvideo",
        "hhanclub",
        "nicept",
        "piggo",
        "pthome",
        "pttime",
        "soulvoice",
        "ssd",
        "tjupt",
        "ttg",
    ] {
        let profile = profiles.get(id).unwrap();
        assert!(profile.rss.is_some(), "profile {id} 应有 RSS 映射");
    }
}
