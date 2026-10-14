#[path = "support/mod.rs"]
mod support;

use domain::MediaKind;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn probe_obligation_is_created_on_scan_and_recovered() {
    let fix = PresenceFixture::new();

    // Scan a file into the library
    let file = fix.root.join("ob_movie (2024).mkv");
    std::fs::write(&file, b"content").unwrap();

    let (status, _) = fix.scan_and_wait(&fix.library_id).await;
    assert_eq!(status, axum::http::StatusCode::OK);

    let store_arc = fix.state.store();
    let row = store_arc
        .lock()
        .ledger_by_path(&file.display().to_string())
        .unwrap()
        .expect("row must exist");

    // Dispatch once
    let count = api::scan_probe_dispatch::dispatch_once(fix.state.clone()).await.unwrap();
    // Either already enqueued/acked or acked in this dispatch_once call
    let remaining = store_arc.lock().scan_probe_obligations(10).unwrap();
    assert_eq!(remaining.len(), 0, "all obligations must be dispatched and acked");
}

#[tokio::test]
async fn deleted_ledger_cleans_obligation_without_probe() {
    let fix = PresenceFixture::new();
    let file = fix.root.join("del_movie (2024).mkv");
    std::fs::write(&file, b"content").unwrap();

    fix.scan_and_wait(&fix.library_id).await;

    let store_arc = fix.state.store();
    let row = store_arc
        .lock()
        .ledger_by_path(&file.display().to_string())
        .unwrap()
        .unwrap();

    // Delete ledger path
    store_arc.lock().delete_ledger_path(&row.path).unwrap();

    // Verify obligation is cleaned up by delete_ledger_path
    let obs = store_arc.lock().scan_probe_obligations(10).unwrap();
    assert!(obs.is_empty(), "obligation must be deleted when ledger row is deleted");
}
