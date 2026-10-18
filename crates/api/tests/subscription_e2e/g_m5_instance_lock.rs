//! G-M5: two instances on one data directory: only the first runs background
//! jobs; the second is read-only until the first shuts down (lease released).

use std::sync::Arc;

use api::ApiState;

use super::harness::TOKEN;

fn open(dir: &std::path::Path) -> ApiState {
    ApiState::new(
        api::Store::open(dir.join("data")).unwrap(),
        TOKEN.into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(super::harness::SearchFixture::empty()),
        Arc::new(downloader::MemoryDownloader::new(dir.join("stage"))),
        dir.join("library"),
    )
    .unwrap()
}

#[tokio::test]
async fn second_instance_runs_read_only_until_first_exits() {
    let tmp = tempfile::tempdir().unwrap();
    let first = api::job_loop::spawn_job_loop(open(tmp.path()));
    assert!(!first.is_read_only());
    let second = api::job_loop::spawn_job_loop(open(tmp.path()));
    assert!(
        second.is_read_only(),
        "second instance must not schedule jobs"
    );
    second.shutdown().await.unwrap();
    first.shutdown().await.unwrap();
    let third = api::job_loop::spawn_job_loop(open(tmp.path()));
    assert!(
        !third.is_read_only(),
        "lease released after first instance stopped"
    );
    third.shutdown().await.unwrap();
}

#[test]
fn stale_lock_is_taken_over() {
    let tmp = tempfile::tempdir().unwrap();
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let now = api::job_loop::unix_now();
    let ttl = 60;
    assert_eq!(
        store
            .try_acquire_instance_lock("crashed", now - 600, ttl)
            .unwrap(),
        None
    );
    assert_eq!(
        store.try_acquire_instance_lock("fresh", now, ttl).unwrap(),
        None
    );
    assert_eq!(
        store.try_acquire_instance_lock("third", now, ttl).unwrap(),
        Some("fresh".to_string())
    );
    assert!(!store.heartbeat_instance_lock("crashed", now).unwrap());
}

async fn wait_until(mut check: impl FnMut() -> bool, secs: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if check() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    check()
}

fn fast_lease_timers() {
    // SAFETY: tests only shorten timers; every reader tolerates any value.
    unsafe {
        std::env::set_var("CRAWLER_MEDIA_LEASE_RETRY_MS", "200");
        std::env::set_var("CRAWLER_MEDIA_LEASE_HEARTBEAT_MS", "200");
    }
}

/// H-1: a holder that crashed (no Drop, lease still fresh) is taken over once
/// its lease expires; the new instance does not stay read-only forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crashed_holder_is_taken_over_after_ttl() {
    fast_lease_timers();
    let tmp = tempfile::tempdir().unwrap();
    let state = open(tmp.path());
    let now = api::job_loop::unix_now();
    // Crashed 57s ago with a 60s TTL: still "live" at startup.
    assert_eq!(
        state
            .store()
            .lock()
            .try_acquire_instance_lock("crashed", now - 57, 60)
            .unwrap(),
        None
    );
    let handle = api::job_loop::spawn_job_loop(state);
    assert!(handle.is_read_only(), "lease still held at startup");
    assert!(
        wait_until(|| !handle.is_read_only(), 15).await,
        "instance must take over once the crashed lease expires"
    );
    handle.shutdown().await.unwrap();
}

/// H-1: when another instance takes the lease over, this one stops its
/// background jobs and rejects manual runs with 409.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lease_loss_stops_jobs_and_gates_manual_run() {
    fast_lease_timers();
    let h = super::harness::Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    let handle = api::job_loop::spawn_job_loop(h.state.clone());
    assert!(!handle.is_read_only());
    let later = api::job_loop::unix_now() + 120;
    assert_eq!(
        h.state
            .store()
            .lock()
            .try_acquire_instance_lock("intruder", later, 60)
            .unwrap(),
        None,
        "intruder takes over"
    );
    assert!(
        wait_until(|| handle.is_read_only(), 10).await,
        "lease loss must degrade to read-only"
    );
    let (status, _) = h.run_subscribe_raw(id).await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT);
    // Review L-9: every write endpoint is gated, reads stay available.
    for (method, path) in [
        ("DELETE", format!("/api/v1/subscriptions/{id}")),
        ("POST", "/api/v1/downloaders/tasks/remove".to_string()),
    ] {
        let response = tower::ServiceExt::oneshot(
            h.app.clone(),
            super::common::request(method, &path, Some(super::harness::TOKEN), serde_json::json!({})),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT, "{method} {path}");
    }
    let response = tower::ServiceExt::oneshot(
        h.app.clone(),
        super::common::request(
            "GET",
            &format!("/api/v1/subscriptions/{id}"),
            Some(super::harness::TOKEN),
            serde_json::Value::Null,
        ),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK, "reads stay available");
    assert!(h.state.store().lock().get_subscribe(id).unwrap().is_some(), "not deleted");
    handle.shutdown().await.unwrap();
}
