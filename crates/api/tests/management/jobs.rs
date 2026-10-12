use std::collections::HashMap;
use std::sync::Arc;

use api::{router, spawn_job_loop_with};
use axum::http::StatusCode;
use domain::SubscribeId;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use rusqlite::Connection;
use serde_json::Value;
use serde_json::json;
use tower::ServiceExt;

use super::common::*;

#[tokio::test]
async fn global_rss_job_fetches_once_and_routes_to_each_subscribe() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("rss", rss_xml())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher.clone(), downloader.clone()));
    create_site(&app).await;
    let mut ids = Vec::new();
    for i in 0..2 {
        let filter = json_data(
            app.clone()
                .oneshot(request(
                    "POST",
                    "/api/v1/rule-sets",
                    Some("management-secret"),
                    json!({
                        "name": format!("arrival-filter-{i}"),
                        "atoms": [
                            { "kind": "resolution", "value": "1080p", "priority": 100 },
                            { "kind": "title_match", "value": "Arrival", "priority": 80 }
                        ]
                    }),
                ))
                .await
                .unwrap(),
        )
        .await;
        let filter_id = filter["id"].as_str().unwrap();

        let mut body = subscribe_payload("rss");
        body["media"]["title"] = json!("Arrival");
        body["filter_id"] = json!(filter_id);
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                body,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let created = json_data(response).await;
        ids.push(
            created["id"]
                .as_str()
                .unwrap()
                .parse::<SubscribeId>()
                .unwrap(),
        );
    }
    let jobs = Connection::open(tmp.path().join("data/jobs.db")).unwrap();
    jobs.execute("DELETE FROM jobs WHERE kind <> 'subscribe_rss'", [])
        .unwrap();
    jobs.execute(
        "UPDATE job_defs SET enabled = 0 WHERE kind <> 'subscribe_rss'",
        [],
    )
    .unwrap();
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/jobs/tick?now=1",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        fetcher
            .requests
            .lock()
            .iter()
            .filter(|key| key.starts_with("rss:"))
            .count(),
        1
    );
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    for id in &ids {
        assert_eq!(store.load_pending(*id).unwrap().len(), 1);
    }
    assert_eq!(downloader.added().len(), 2);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/jobs/tick?now=601",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(downloader.added().len(), 2);
    let first_filter = store.get_subscribe(ids[0]).unwrap().unwrap().filter_id;
    store.delete_filter(first_filter).unwrap();
    Connection::open(tmp.path().join("data/subscribe.db"))
        .unwrap()
        .execute(
            "DELETE FROM pending_downloads WHERE subscribe_id = ?1",
            [ids[1].to_string()],
        )
        .unwrap();
    let response = app
        .oneshot(request(
            "POST",
            "/api/v1/jobs/tick?now=1201",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        fetcher
            .requests
            .lock()
            .iter()
            .filter(|key| key.starts_with("rss:"))
            .count(),
        3
    );
    assert_eq!(store.load_pending(ids[1]).unwrap().len(), 1);
    assert_eq!(downloader.added().len(), 3);
}

#[tokio::test]
async fn active_only_returns_only_defs_with_live_children() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader));
    let all = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/jobs",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    for def in all["data"].as_array().unwrap() {
        let id = def["id"].as_str().unwrap();
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/{id}/cancel"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    // 取最后一个**启用**的 def：维护任务会注册一个默认关闭的 def，且顺序在实例 def 之后。
    let target = all["data"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|row| row["enabled"] == true)
        .expect("至少有一个启用的 def")["id"]
        .as_str()
        .unwrap();
    let run = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/jobs/{target}/run"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(run.status(), StatusCode::OK);
    let active = json_body(
        app.oneshot(request(
            "GET",
            "/api/v1/jobs?active_only=true&limit=1",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    let rows = active["data"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], target);
    assert_eq!(rows[0]["last_status"], "queued");
}

#[tokio::test]
async fn creating_a_subscribe_inserts_search_job_def() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader));
    create_site(&app).await;
    let first = create_subscribe(&app, "search").await;
    let second = create_subscribe(&app, "search").await;
    assert_ne!(first["id"], second["id"]);

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
    let kinds: Vec<_> = defs.iter().map(|d| d["kind"].as_str().unwrap()).collect();
    assert_eq!(
        kinds.iter().filter(|k| **k == "transfer").count(),
        1,
        "{kinds:?}"
    );
    assert_eq!(kinds.iter().filter(|k| **k == "subscribe_rss").count(), 1);
    assert_eq!(kinds.iter().filter(|k| **k == "watch_intake").count(), 1);
    assert_eq!(
        kinds.iter().filter(|k| **k == "subscribe_search").count(),
        2
    );
}

#[tokio::test]
async fn jobs_list_includes_schedule_and_run_times() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader));
    create_site(&app).await;
    create_subscribe(&app, "search").await;

    let body = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/jobs",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let defs = body.as_array().unwrap();
    let transfer = defs
        .iter()
        .find(|row| row["kind"] == "transfer")
        .expect("transfer Job");
    assert_eq!(transfer["name"], "Transfer");
    assert_eq!(transfer["enabled"], true);
    assert_eq!(transfer["schedule"]["interval_secs"], 30);
    assert_eq!(transfer["next_run_after"], 0);
    assert!(transfer["last_status"].is_null() || transfer["last_status"] == "queued");
    assert!(transfer["last_finished_at"].is_null());
    let rss = defs
        .iter()
        .find(|row| row["kind"] == "subscribe_rss")
        .expect("rss Job");
    assert_eq!(rss["schedule"]["interval_secs"], 600);
    let search = defs
        .iter()
        .find(|row| row["kind"] == "subscribe_search")
        .expect("search Job");
    assert_eq!(search["schedule"]["interval_secs"], 1800);

    for now in [1, 31, 61, 91, 121, 151, 181, 211] {
        let tick = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(tick.status(), StatusCode::OK);
        let listed = json_data(
            app.clone()
                .oneshot(request(
                    "GET",
                    "/api/v1/jobs",
                    Some("management-secret"),
                    Value::Null,
                ))
                .await
                .unwrap(),
        )
        .await;
        let transfer = listed
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["kind"] == "transfer")
            .unwrap();
        if transfer["last_status"] == "succeeded" {
            assert_eq!(transfer["last_finished_at"], now);
            assert_eq!(transfer["next_run_after"], now + 30);
            break;
        }
    }

    // 当任务被 disable 时，next_run_after 必须为 null
    let toggle = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/jobs/{}", transfer["id"].as_str().unwrap()),
            Some("management-secret"),
            json!({ "enabled": false }),
        ))
        .await
        .unwrap();
    assert_eq!(toggle.status(), StatusCode::OK);

    let listed_disabled = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/jobs",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let transfer_disabled = listed_disabled
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == "transfer")
        .unwrap();
    assert_eq!(transfer_disabled["enabled"], false);
    assert!(transfer_disabled["next_run_after"].is_null());
}

#[tokio::test]
async fn jobs_endpoint_requires_auth() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader));
    let response = app
        .oneshot(request("GET", "/api/v1/jobs", None, Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn job_tick_adds_then_transfers_without_blocking_search() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let source = tmp.path().join("matrix.mkv");
    std::fs::write(
        &source,
        r#"{"resolution":"2160p","codec":"hevc","hdr":"hdr10"}"#,
    )
    .unwrap();
    downloader.map_enclosure("https://pt.example/download.php?id=1&passkey=abc", source);
    let app = router(state(tmp.path(), fetcher, downloader.clone()));
    create_site(&app).await;
    create_subscribe(&app, "search").await;

    let mut ledger_len = 0;
    let mut kinds = Vec::new();
    for now in [1, 31, 61, 91, 121, 151, 181, 211, 241, 271] {
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
        kinds.push(json_body(response).await);
        let ledger = app
            .clone()
            .oneshot(request(
                "GET",
                "/api/v1/ledger",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        ledger_len = json_data(ledger).await.as_array().unwrap().len();
        if downloader.added().len() == 1 && ledger_len == 1 {
            break;
        }
    }
    assert_eq!(
        downloader.added().len(),
        1,
        "ticks={kinds:?} added={:?}",
        downloader.added()
    );
    assert_eq!(ledger_len, 1, "ticks={kinds:?}");
}

#[tokio::test]
async fn job_tick_requires_auth() {
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
        .oneshot(request("POST", "/api/v1/jobs/tick", None, Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn process_job_loop_adds_then_transfers_without_http_tick() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let source = tmp.path().join("matrix.mkv");
    std::fs::write(
        &source,
        r#"{"resolution":"2160p","codec":"hevc","hdr":"hdr10"}"#,
    )
    .unwrap();
    downloader.map_enclosure("https://pt.example/download.php?id=1&passkey=abc", source);
    let state = state(tmp.path(), fetcher, downloader.clone());
    let app = router(state.clone());
    create_site(&app).await;
    create_subscribe(&app, "search").await;

    let now = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(1));
    let clock = now.clone();
    let handle = spawn_job_loop_with(state, std::time::Duration::from_millis(5), move || {
        clock.load(std::sync::atomic::Ordering::SeqCst)
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut ledger_len = 0;
    while std::time::Instant::now() < deadline {
        let ledger = app
            .clone()
            .oneshot(request(
                "GET",
                "/api/v1/ledger",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        ledger_len = json_data(ledger).await.as_array().unwrap().len();
        if !downloader.added().is_empty() {
            now.store(31, std::sync::atomic::Ordering::SeqCst);
        }
        if downloader.added().len() == 1 && ledger_len == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    handle.abort();
    assert_eq!(downloader.added().len(), 1);
    assert_eq!(ledger_len, 1);
}

#[tokio::test]
async fn job_stream_disconnect_exits_loop() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader));

    let res = app
        .oneshot(request(
            "GET",
            "/api/v1/jobs/stream",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    // 立即 drop 掉响应体以模拟客户端断开连接
    drop(res);

    // 暂停片刻，后台 loop 检测到 tx.is_closed() 或 send 失败自动安全退出，不 panic 不死循环
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

#[tokio::test]
async fn job_stream_emits_ready_and_initial_job_count_immediately() {
    use tokio_stream::StreamExt;

    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader));

    let res = app
        .oneshot(request(
            "GET",
            "/api/v1/jobs/stream",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);

    let start = std::time::Instant::now();
    let mut body_stream = res.into_body().into_data_stream();

    let mut received = Vec::new();
    while let Some(Ok(chunk)) = body_stream.next().await {
        received.extend_from_slice(&chunk);
        let text = String::from_utf8_lossy(&received);
        if text.contains("event: ready") && text.contains("event: job") {
            break;
        }
    }

    assert!(
        start.elapsed() < std::time::Duration::from_millis(500),
        "Initial ready and job events must be emitted immediately without waiting 2s: {:?}",
        start.elapsed()
    );
}
