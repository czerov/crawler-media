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
