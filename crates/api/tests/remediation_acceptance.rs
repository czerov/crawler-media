#[path = "support/mod.rs"]
mod support;
#[path = "support/remediation.rs"]
mod remediation;

use axum::http::StatusCode;

#[tokio::test]
async fn slow_io_and_maintenance_keep_auth_and_heartbeat_available() {
    let fixture = remediation::fixture();
    let gate = remediation::Gate::new();
    let _release = gate.release_guard();
    let worker_gate = gate;
    let worker = std::thread::spawn(move || worker_gate.wait());

    // Characterization only: management fixture uses temporary storage and fake external work.
    let (auth_status, _) = fixture
        .request("GET", "/api/v1/libraries", Some(&fixture.admin_token))
        .await;
    assert_eq!(auth_status, StatusCode::OK);
    let (unauth_status, _) = fixture.request("GET", "/api/v1/libraries", None).await;
    assert_eq!(unauth_status, StatusCode::UNAUTHORIZED);

    // Releasing the guard unblocks the worker; if an earlier assertion panics, Drop does so.
    drop(_release);
    worker.join().expect("released worker must finish");
}

#[tokio::test]
async fn cancel_and_restart_do_not_expand_worker_budgets_or_lose_jobs() {
    let fixture = remediation::fixture();
    // The public budgets are observable, but R3/R4 lifecycle/drain seams are not yet present.
    // This is a characterization of current request admission, not a green recovery claim.
    assert_eq!(fixture.state.search_work.available_permits(), 4);
    assert_eq!(fixture.state.fs_work.available_permits(), 2);
    assert_eq!(fixture.state.http_work.available_permits(), 8);
}

#[tokio::test]
async fn scoped_pages_remain_equivalent_under_background_changes() {
    let fixture = remediation::fixture();
    let (initial_status, initial) = fixture
        .request("GET", "/api/v1/libraries", Some(&fixture.member_token))
        .await;
    assert_eq!(initial_status, StatusCode::OK);
    let visible_ids = initial["data"]
        .as_array()
        .expect("library data is an array")
        .iter()
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert!(visible_ids.iter().any(|id| id == &fixture.library_id));

    // An admin-scoped read cannot change subsequent member authorization results.
    let (admin_status, _) = fixture
        .request("GET", "/api/v1/libraries?scope=admin", Some(&fixture.admin_token))
        .await;
    assert_eq!(admin_status, StatusCode::OK);
    let (repeat_status, repeated) = fixture
        .request("GET", "/api/v1/libraries", Some(&fixture.member_token))
        .await;
    assert_eq!(repeat_status, StatusCode::OK);
    let repeated_ids = repeated["data"]
        .as_array()
        .expect("library data is an array")
        .iter()
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert_eq!(repeated_ids, visible_ids);
}

#[test]
#[ignore = "R3/R4 pending: public drain/recovery seams are not available"]
fn r3_r4_durable_queue_capacity_and_drain_are_behaviorally_red() {
    // Deliberately ignored until R3/R4 expose public lifecycle and queue statistics.
}
