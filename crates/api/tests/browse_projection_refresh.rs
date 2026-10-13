#[path = "support/mod.rs"]
mod support;

use domain::MediaKind;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn projection_service_single_flight_builds_and_retrieves() {
    let fix = PresenceFixture::new();

    // Add a media item
    let (_m1, _r1) = fix.insert_media_and_ledger("test1 (2024).mkv");

    // Ensure projection
    let snap = fix
        .state
        .projection
        .ensure(fix.state.clone(), fix.library_id.clone())
        .await
        .unwrap();

    assert_eq!(snap.library_id, fix.library_id);
    assert_eq!(snap.media.len(), 1);
    assert_eq!(snap.files.len(), 1);

    // Concurrent ensure shares the same build
    let (s1, s2) = tokio::join!(
        fix.state.projection.ensure(fix.state.clone(), fix.library_id.clone()),
        fix.state.projection.ensure(fix.state.clone(), fix.library_id.clone()),
    );
    assert!(s1.is_ok());
    assert!(s2.is_ok());
}
