use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use rusqlite::Connection;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;

#[tokio::test]
async fn jobs_list_includes_check_in_def() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/jobs",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let defs = json_data(response).await;
    let defs = defs.as_array().unwrap();
    assert!(defs.iter().any(|row| row["kind"] == "check_in"), "{defs:?}");
}

#[tokio::test]
async fn job_tick_runs_check_in_when_a_site_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    create_site(&app).await;

    let mut saw_check_in = false;
    for now in [1, 31, 61, 91, 121, 151, 181, 211] {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_data(response).await;
        let kinds = body["kinds"].as_array().unwrap();
        if kinds.iter().any(|kind| kind == "check_in") {
            saw_check_in = true;
            break;
        }
    }
    assert!(saw_check_in, "Check-in Job did not run");
}

#[tokio::test]
async fn failed_site_check_in_fails_the_scheduled_job() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let mut site = site_payload();
    site["cookie"] = Value::Null;
    site["url"] = Value::String("http://127.0.0.1:1".into());
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/sites",
            Some("management-secret"),
            site,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let mut saw_check_in = false;
    for now in [1, 31, 61, 91, 121, 151, 181, 211] {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        let body = json_data(response).await;
        if body["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|kind| kind == "check_in")
        {
            saw_check_in = true;
            break;
        }
    }
    assert!(saw_check_in, "Check-in Job did not run");

    let jobs = Connection::open(tmp.path().join("data/jobs.db")).unwrap();
    let (status, error): (String, String) = jobs
        .query_row(
            "SELECT status, error FROM jobs WHERE kind = 'check_in' ORDER BY run_after DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "queued");
    assert!(
        error.contains("请人工登录并配置有效 Cookie"),
        "缺 Cookie 必须给出可操作失败，而不是假装连接远端: {error}"
    );
}

#[tokio::test]
async fn site_check_in_endpoint_runs_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let site = create_site(&app).await;
    let response = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/sites/{}/check-in", site["id"].as_str().unwrap()),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["ok"], false);
    assert!(
        body["error"]["message"].as_str().unwrap_or_default().contains("Cookie"),
        "占位站点不得报告签到成功: {body}"
    );
}

#[tokio::test]
async fn site_login_endpoint_runs_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let site = create_site(&app).await;
    let response = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/sites/{}/login", site["id"].as_str().unwrap()),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert_eq!(body["ok"], false);
    assert!(
        body["error"]["message"].as_str().unwrap_or_default().contains("Cookie"),
        "空登录请求不得伪造会话: {body}"
    );
}

#[tokio::test]
async fn site_actions_reject_unknown_site() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let response = app
        .oneshot(request(
            "POST",
            "/api/v1/sites/00000000-0000-0000-0000-0000000000ff/check-in",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn browser_maintenance_falls_back_to_http_when_browser_fails() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_c = stop.clone();
    std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        while !stop_c.load(Ordering::Relaxed) {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_nonblocking(false);
                let mut buf = [0; 1024];
                let _ = stream.read(&mut buf);
                let body = "<html><body><p>签到成功</p><a href=\"logout.php\">Logout</a></body></html>";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });

    let tmp = tempfile::tempdir().unwrap();
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader,
    );

    let site = domain::Site {
        id: domain::SiteId::new(),
        name: "test-site".into(),
        url: format!("http://127.0.0.1:{port}/"),
        profile_id: "demo".into(),
        cookie: Some("old=1".into()),
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: Some("http://127.0.0.1:9222".into()),
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Browser,
    };
    state.store().lock().insert_site(&site).unwrap();
    state.store().lock().put_setting(api::settings_keys::CDP_ENABLED, "1").unwrap();

    let app = router(state);
    let response = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/sites/{}/check-in", site.id),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();

    stop.store(true, Ordering::Relaxed);
    let status = response.status();
    let body = json_body(response).await;
    assert_eq!(status, StatusCode::OK, "response was: {body}");
}
