#[path = "support/mod.rs"]
mod support;

use domain::MediaKind;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn scan_seen_checkpoints_and_avoids_reprocessing() {
    let fix = PresenceFixture::new();

    let f1 = fix.root.join("Movie1 (2024).mkv");
    let f2 = fix.root.join("Movie2 (2024).mkv");
    std::fs::write(&f1, b"1").unwrap();
    std::fs::write(&f2, b"2").unwrap();

    let store_arc = fix.state.store();
    let op = {
        let guard = store_arc.lock();
        guard.enqueue_scan(&fix.library_id, "sig", &[], 1000).unwrap()
    };

    // Checkpoint f1 as seen
    {
        let guard = store_arc.lock();
        guard.record_scan_seen(&op.id, &[f1.clone()]).unwrap();
        assert!(guard.is_scan_seen(&op.id, &f1.display().to_string()).unwrap());
        assert!(!guard.is_scan_seen(&op.id, &f2.display().to_string()).unwrap());
    }

    // Clear seen
    {
        let guard = store_arc.lock();
        guard.clear_scan_seen(&op.id).unwrap();
        assert!(!guard.is_scan_seen(&op.id, &f1.display().to_string()).unwrap());
    }
}
